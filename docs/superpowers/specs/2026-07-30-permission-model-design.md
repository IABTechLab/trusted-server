# Design Spec: Jurisdiction Permission Model

**Status:** Draft
**Author:** Engineering
**Issue references:** #779
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-provider-migration-rollout-design.md`
**Last updated:** 2026-07-31

> **Context.** PR #838 proposed a permission model whose review surfaced two
> classes of defect this spec exists to prevent in the next pass: (1) silent
> behavioral inversions of consent-signal precedence — most seriously, a
> present TCF string short-circuiting GPC/GPP/US-Privacy opt-outs — and
> (2) fail-open jurisdiction resolution when geolocation is disabled. The
> precedence rules (§4) and the failure-mode matrix (§6) are the two
> documents whose absence allowed those defects to hide in a 67-file diff.
> They are normative: an implementation whose behavior differs from these
> tables is wrong, whatever its tests say. This spec also reverses one
> PR #838 structural decision: policy lives in `trusted-server.toml`, not in
> a build-time-embedded YAML file (§3.1).

---

## 1. Overview

The permission model replaces the hard-wired jurisdiction gate
(`allows_ec_creation` and its companions) with a single resolved
**permission set** per request. Every data decision Trusted Server itself
makes — EC provider execution, EC creation and withdrawal, and EID
transmission into the bidstream — reads that set (§7).

The set is resolved from three inputs:

1. **Jurisdiction** — the country/region the request resolves to (§5).
2. **Policy** — a declarative map from jurisdiction to a baseline
   acquisition rule per permission (§3).
3. **Signals** — the request's privacy signals: TCF, GPP, GPC, US Privacy
   (§4).

These are the initial sources. #777/#779 also envision publisher
interaction and external services as permission sources; that
source-interface is **explicitly deferred**, not silently dropped — §10
records the divergence, and adding a source later means adding a grant- or
opt-out-class input to §4's taxonomy, not a new resolution algorithm.

Scope: the model governs decisions Trusted Server makes. Downstream RTB
partners receive the full, unmodified regulatory context and make their own
compliance decisions.

## 2. Vocabulary: enforced permissions only

Permissions are named by IAB TCF Europe purpose identifiers, used strictly as
technical identifiers (no CMP or TCF policy is implemented by naming them).

**Rule: a purpose appears in the model only when it has both a signal mapping
and an enforcement point.** PR #838 shipped 11 purposes of which 9 were
inert — computed into the bitset and consumed by nothing but a startup log —
while the policy file invited operators to set flags (e.g.
`market-research: denied`) that changed nothing. A policy vocabulary that
overstates what is enforced is a compliance hazard, not forward
compatibility.

The initial vocabulary is therefore exactly:

| Identifier                | TCF purpose | Enforcement points                                                                                                                                                                           |
| ------------------------- | ----------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `store-on-device`         | 1           | EC provider execution; EC creation; withdrawal/tombstone eligibility                                                                                                                         |
| `select-personalised-ads` | 4           | All bidstream and partner identity egress — raw EC in `user.id`, derived request IDs, page bids, EIDs, identify, pull/batch sync (jointly with `store-on-device`; the full path table is §7) |

(The identifier strings are the IAB names verbatim, including their original
spelling.) The extension procedure — add the signal mapping, add the
enforcement point, add the policy vocabulary entry, in one change — is
documented alongside the policy schema. Policy validation **rejects** a rule
that references an identifier outside the current vocabulary, so a policy can
never promise more than the code enforces.

## 3. Policy

### 3.1 Location: `[permissions]` in `trusted-server.toml`

Policy is operator-owned runtime configuration, expressed as a
`[permissions]` section of `trusted-server.toml`, flowing through the same
pipeline as every other setting (`ts config push` publishes it as part of the
config blob envelope; instances pick it up like any config change).

This deliberately reverses PR #838, which embedded a `permissions.yaml` at
build time via `include_str!`. That design was rejected because:

- a policy edit — the operation the whole model exists to make easy —
  required recompiling and redeploying the binary, cutting against the
  runtime config-store pipeline the project has standardized on;
- it introduced a second configuration language and a second validation
  path next to the TOML settings machinery that already exists;
- the `include_str!` reached two directory levels above the crate root,
  breaking crate packaging;
- validation ran lazily at first use behind a `OnceLock` + `expect`, so a
  bad edit that escaped unit tests became a 500 on every request.

Auditability is preserved where it actually lives: the source-controlled
`trusted-server.example.toml` ships the complete recommended policy table
(the reviewable reference artifact), and the operator's own config history —
git for the file, config-store versions for pushes — is the change log.

**Compiled-in fallback:** when a config has no `[permissions]` section, a
minimal compiled-in policy applies in which **every permission is
`requires_signal` for every jurisdiction**, with **`regime = "gdpr"`** so
auction dispatch (§7) is defined and maximally protective too. (This is
the most protective posture that still admits consent — `denied` would be
stricter but would make a signal-carrying deployment inoperable by
default; the distinction is stated, not glossed.) Absence of policy is
always safe; there is no fail-open default.

### 3.2 Format

Named **groups** (baselines) and **rules** mapping a country (`FR`) or
country/state pair (`"US/CA"`) to a group, with optional per-permission
overrides. Each permission resolves to an **acquisition rule**:

- `granted` — set without any signal,
- `requires_signal` — set only when a signal grants it (opt-in),
- `denied` — never set, even when a signal grants it.

```toml
[permissions.groups.gdpr-eu]
regime = "gdpr"
default = "requires_signal"

# Opt-out regime, expressed as requires_signal: explicit non-opt-out
# values are grant-class signals (§4), so signal-carrying traffic is
# granted while no-signal traffic stays blocked — matching today's US
# behavior, which `granted` cannot express.
[permissions.groups.us-opt-out]
regime = "us-privacy"
default = "requires_signal"

[permissions.groups.non-regulated]
regime = "none"
default = "granted"

[permissions.rules]
FR = "gdpr-eu"
US = "us-opt-out"
# Overrides name explicit acquisition rules — no +/- sigil syntax; TOML
# expresses the target state directly.
"US/CA" = { group = "us-opt-out", overrides = { select-personalised-ads = "requires_signal" } }
# Reserved key: countries that resolve but match no rule. Required whenever
# the [permissions] section is present. Distinct from [geo] default_country,
# which handles requests that resolve no country at all (§5.4).
default = "non-regulated"
```

A group's `default` covers unlisted permissions; a group may also name
permissions explicitly. Overrides map identifier → acquisition rule, so any
target state (including `requires_signal`) is expressible — PR #838's
`+`/`-` sigil scheme could not express "requires a signal", the most common
real-world override.

Each group carries a required **`regime`** class (`gdpr`, `us-privacy`, or
`none`). This is the explicit legal-classification channel: consumers that
need a jurisdiction _class_ — above all server-side auction dispatch — read
`regime`, never infer a class from purpose flags. Inference is lossy
(Purpose 1 and Purpose 4 may legitimately carry different rules, and a
non-GDPR operator may choose an opt-in Purpose 4) and would smuggle legal
meaning back into identifiers this spec declares purely technical (§2).

### 3.3 Validation — at config acceptance, not request time

Policy is validated where every other setting is: at `ts config push` (a bad
policy is rejected before publication) and at settings construction on
startup (a bad stored config produces the startup-error state, never a
per-request failure).

Validation rejects:

- unknown fields anywhere (`deny_unknown_fields` on every deserialized
  struct — PR #838's untagged rule type silently swallowed a misspelled
  override key, dropping the operator's override with no diagnostic);
- rule keys whose country part is not in the embedded **assigned** ISO
  3166-1 alpha-2 list (not merely `[A-Z]{2}` — an unassigned code is
  almost certainly a typo silently diverting a country to the fallback);
  the region part matches `[A-Z0-9]{1,3}`. The `US/CA` slash form is the
  house rule-key format corresponding to ISO 3166-2 `US-CA`;
- references to permissions outside the enforced vocabulary (§2);
- references to undefined groups, and groups missing the `regime` class;
- groups that neither list every permission nor provide `default`;
- a present `[permissions]` section without a `rules.default` entry (§5.4
  depends on it existing — its absence must be a validation error, not a
  runtime surprise);
- an empty `[permissions]` section (ambiguous intent: an operator who wants
  the compiled-in fallback omits the section entirely);
- duplicate rule keys under case-insensitive comparison (`FR` and `fr`);
- a `[geo] default_country` whose country part is not an assigned ISO
  code; it accepts either a country (`FR`) or a country/region key
  (`US/CA`) — PR #838 supported region defaults, and a no-geo,
  single-state deployment must be able to select its state rule. It is
  canonicalized to uppercase, and startup logs which rule (or
  `rules.default`) it resolves to.

### 3.4 One source of jurisdiction truth

Today, `detect_jurisdiction` — driven by the runtime lists
`consent.gdpr.applies_in` and `consent.us_privacy.states` — is the sole
jurisdiction source for **both** the auction consent gate and the EC gate.
The permission model replaces the EC side; if the auction gate keeps reading
the old lists while EC reads policy rules, the two will drift (adding a
country to one has no effect on the other, and an operator has no signal
that they disagree).

Requirement: the auction gate's jurisdiction class derives from the same
resolved policy, reading the rule's explicit **`regime`** class (§3.2) — a
country is GDPR-class when its rule resolves to a `regime = "gdpr"` group.
The class is never inferred from purpose flags. Where the legacy lists must
survive an interim period, a CI test asserts consistency between each list
and the policy's regime classes, with deliberate divergences recorded as
explicit, commented exceptions in the test — never silent. Both legacy
lists are in scope, not only the GDPR one.

### 3.5 Shipped-table coverage

A CI test asserts every member of the GDPR country list resolves to a
GDPR-class baseline in the example policy. This closes a defect class
nothing in PR #838's validation covered: a mistyped country key (`DL:` for
`DK:`) parses cleanly, starts cleanly, and silently drops a member state to
the fallback rule. Countries intentionally unlisted are governed by the
`rules.default` entry (§3.2); the example policy documents that fallback
inline, and ships it as the most protective baseline.

## 4. Signal precedence — normative

Signals are classified into three classes — a two-class model (TCF grant /
opt-out) cannot reproduce today's US behavior, where no-signal traffic is
blocked but an **explicit non-opt-out** value grants:

- **Opt-out signals** (affirmative withdrawal): GPC header; GPP sections
  carrying a sale/sharing opt-out; US Privacy opt-out. Opt-out signals are
  honored **globally**, not only in the jurisdictions whose law defines
  them — a deliberate, more-protective simplification: scoping a browser's
  explicit opt-out to a geolocation guess would honor it for some visitors
  and ignore it for others based on IP evidence. (For jurisdictions outside
  US states this is a declared behavior change; migration spec §2 records
  it.)
- **Grant signals** (affirmative permission): a decodable TCF record
  consenting to the purpose; an **explicit GPP non-opt-out value** (e.g.
  `sale_opt_out = false`); a **US Privacy string present and not opting
  out** — including the "not applicable" flag, which today's tests pin as
  allowing. Any grant signal satisfies a `requires_signal` baseline; this
  is what lets a `requires_signal` US rule preserve today's "no signal →
  block, explicit non-opt-out → allow" behavior, which neither `granted`
  nor a TCF-only grant class could express.
- **Refusals**: a decodable TCF record refusing the purpose. A refusal is
  neither a grant nor an opt-out — it blocks acquisition (precedence 3)
  and withdraws only per §4.2.

**Precedence, highest first:**

1. Policy `denied` — never set, regardless of any signal.
2. **Opt-out signal — always revokes**, regardless of any consent record
   present. A GPC header revokes `store-on-device` and
   `select-personalised-ads` even when an accompanying TCF string consents to
   them. _(This is the rule PR #838 inverted: its resolution returned from
   inside the TCF branch before ever reaching the opt-out check, so a
   consenting CMP string made the browser's GPC signal a no-op — a
   CCPA-facing regression. The pre-existing tests pinning this rule —
   `ec_blocked_us_state_gpc_overrides_tcf` and companions — are reinstated
   against the new API, not deleted.)_
3. Consent record refusal — a TCF record present and refusing the purpose
   revokes it. This applies in **every** jurisdiction, including
   `granted`-baseline ones: an expressed refusal always beats a policy
   default. Note this is a declared, more-protective divergence from the
   pre-epic gate, which ignored consent records entirely outside regulated
   jurisdictions — the migration spec's matrix (row 6) records it. Refusal
   revokes new grants only; whether it also destroys existing identity is
   governed strictly by §4.2.
4. Grant signal — any grant-class signal (TCF consent, explicit GPP
   non-opt-out, present-and-not-opted-out US Privacy) grants the permission
   (subject to 1–3: a coexisting TCF refusal beats a non-TCF grant signal,
   matching today's US-state ordering where a present TCF record decides
   before GPP/USP values are consulted).
5. No signal — the policy baseline decides: `granted` sets it,
   `requires_signal` leaves it unset.

### 4.1 Decision matrix

For each enforced permission, with baseline _B_ ∈ {granted,
requires_signal, denied}:

| Opt-out present | TCF refusal present | Grant signal present | Result                                           |
| --------------- | ------------------- | -------------------- | ------------------------------------------------ |
| yes             | —                   | —                    | **unset** (and withdrawal semantics apply, §4.2) |
| no              | yes                 | —                    | unset (withdrawal per §4.2, trigger 2)           |
| no              | no                  | yes                  | set, unless B = denied                           |
| no              | no                  | no                   | set iff B = granted                              |

### 4.2 Withdrawal vs. absence

Withdrawal (destructive: expire the EC cookie, write revocation tombstones)
and non-grant (the permission is simply unset) are distinct outcomes, never
conflated. "Baseline" below always means the **resolved acquisition rule for
`store-on-device` in the request's jurisdiction, after overrides** — never a
group label, since a group can mix rules across permissions.

The triggers, exhaustively — nothing else withdraws:

1. **An opt-out signal withdraws in every jurisdiction, whatever the
   baseline.** (For US states this preserves today's behavior; elsewhere it
   is the declared change of §4's global-opt-out rule.)
2. **A TCF record refusing `store-on-device` withdraws iff the baseline is
   `requires_signal` or `denied`.** Where the baseline is `granted`,
   refusal blocks _new_ grants but never tombstones: tombstones are
   irreversible, and PR #838 wrote them for visitors in unregulated
   jurisdictions whose global CMP emitted a purpose-refusing string —
   permanent identity loss under a regime the deployment never opted into.
   (The `denied` arm exists so trigger 3 is coherent: after a policy
   tightens to `denied`, an affirmative refusal must still be able to
   withdraw a pre-existing identity.)
3. **A policy edit is not a user signal.** Tightening a baseline to
   `denied` stops new identity but does not itself tombstone identities
   minted before the change; cleaning those up is an operational action
   (migration spec §6). An affirmative user signal (trigger 1, or trigger 2
   under the now-`denied` baseline) still withdraws them — with a test
   pinning exactly this sequence: existing EC → policy tightens to
   `denied` → refusal arrives → tombstone.
4. **Absence of signal never destroys identity.** A visitor who has not yet
   made a choice is never stripped of an existing identity.

Withdrawal checking follows §4 precedence: an opt-out signal triggers
withdrawal even when a consenting TCF record is present.
`ec_storage_withdrawn` (or its successor) gets direct unit coverage for
every trigger above; in PR #838 the headline "withdrawal expires identity"
behavior had no unit test at all.

### 4.3 Withdrawal durability

Withdrawal is two writes — the KV tombstones and the cookie expiry — and
the contract for partial failure is explicit (PR #838 expired the cookie
first and logged-and-swallowed tombstone-write failures, which can leave a
live graph identity with no browser handle pointing at it):

- **Order: tombstones first, cookie expiry second.** The cookie is expired
  only after the tombstone writes succeed.
- **On tombstone-write failure, the cookie is left in place** and the
  failure is logged at `error` with a metric. This is deliberately
  self-healing: every withdrawal trigger is durable client-side (GPC is a
  browser setting, the TCF record lives in the CMP's storage), so the next
  request re-presents the signal and retries the whole withdrawal. No
  quarantine queue is needed; the browser is the retry queue.
- **Partial progress is explicit, not atomic.** The tombstone writes are a
  **revocation family**: an enumerable, ordered set of independent KV
  writes (cookie hash, active-EC hashes), each idempotent, with no
  multi-key atomicity assumed — the current storage offers none, and the
  spec does not pretend otherwise. A retry resumes the family from the
  start (idempotent writes make re-writing completed members harmless).
  Withdrawal is **complete** only when every family member is committed;
  the cookie expires only then.
- **Reads fail closed on partial families**: every consumer (identify,
  batch-sync, pull-sync, egress gates) treats an identity as revoked when
  **any** member of its revocation family is present — a partially
  withdrawn identity is unusable immediately, even before the family
  completes.
- Fault-injection tests cover failure at the **Nth** family write (not
  only total failure): first write lands, second fails → cookie untouched,
  identity already treated as revoked by readers, error logged; subsequent
  request with the same signal → family completes and the cookie expires.

### 4.4 Signal normalization — normative matrix

§4's precedence operates on normalized inputs: one effective consent
record and one effective opt-out state per request. The normalization
layer is where today's real-world mess lives, and PR #838 collapsed it
silently. These are the outcomes — decided here, not delegated to the
implementation; each row marked **changed** also appears in the migration
matrix:

| Input state                                                                  | Effective record / outcome                                                                                                                                                                                                                                                                                                                                         | Status                                                  |
| ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------- |
| Standalone TCF and GPP-embedded TCF disagree per purpose, mode `restrictive` | Per-purpose **synthesis**: a purpose is consented only when **both** records consent (AND)                                                                                                                                                                                                                                                                         | Preserved (mode semantics pinned against current tests) |
| Same, mode `permissive`                                                      | Per-purpose synthesis: consented when **either** record consents (OR)                                                                                                                                                                                                                                                                                              | Preserved (same pinning)                                |
| Same, mode `newest`                                                          | **Whole-record selection** by `Created` timestamp; tie → the GPP-embedded record                                                                                                                                                                                                                                                                                   | Preserved (same pinning)                                |
| Expired consent record                                                       | Treated as **absent entirely** — grants nothing, refuses nothing, withdraws nothing; the baseline applies. Under a `granted` baseline that means the grant stands: an expired refusal is not current evidence and must not revoke indefinitely                                                                                                                     | Preserved                                               |
| One valid record + a second malformed record of the same family              | The **valid record governs**; the malformed one is ignored with a `warn` log. Fail-closed-on-malformed (below) applies only when no valid record of that family exists                                                                                                                                                                                             | Decided here                                            |
| Persisted-KV consent record, live record present                             | **Live wins**, always; the stored record is never consulted                                                                                                                                                                                                                                                                                                        | Preserved                                               |
| Persisted-KV consent record, no live record                                  | Substitutes as the effective record **iff within the same TTL as a live record**; staler → absent. This narrow read is exempt from the graph-read permission gate (§7) — determining `store-on-device` cannot itself require `store-on-device`                                                                                                                     | Preserved, circularity resolved                         |
| Proxy/mirror mode (CMP state mirrored server-side)                           | Mirror-sourced record enters as a live record; when both the mirror and the request carry records, the **request's record wins** (closer to the user)                                                                                                                                                                                                              | Decided here                                            |
| GPP opt-out fields                                                           | Enumerated per supported section: the sale, sharing, and targeted-advertising opt-out fields each independently constitute an opt-out signal when set; **all present-and-false** constitutes a grant signal (§4); absent/N-A fields contribute nothing. The exact field list per section ID is an appendix of the implementation PR, reviewed against the GPP spec | Decided here                                            |
| Malformed-but-present record, no valid record of that family                 | **Blocks grants** (fail-closed acquisition — it does not degrade to "absent", which under a `granted` baseline would turn garbage into a grant, the fail-open path in both #838 and the first draft of this spec). Never triggers withdrawal — destruction requires an affirmative, decodable signal (§4.2)                                                        | Changed (declared)                                      |

## 5. Jurisdiction resolution

### 5.1 Order

Geo resolution runs **before** permission resolution — jurisdiction is an
input to the permission set, which is why geo providers cannot themselves be
gated on it (providers spec §5). A selected geo provider resolves country
and optional region; rules match `country/region` first, then `country`,
case-insensitively.

### 5.2 Lookup failure

Provider selected, lookup resolves nothing for a request → the configured
`[geo] default_country` rules apply (per #779). An adapter whose geo
implementation can never resolve anything must not accept the selection at
all — that is the capability check of providers spec §6, and it prevents a
"selected but always empty" provider from silently converting every request
to §5.3 semantics without §5.3's guard.

Declared residual: when the default country's baseline is permissive, a
per-request lookup failure is a per-request grant to traffic of unknown
origin — this path is not fail-closed, and the spec does not pretend it is.
The lookup-failure rate is exported as a metric and logged, so an elevated
rate (a degraded geo backend silently converting traffic to the default) is
observable rather than invisible.

### 5.3 No geo provider selected

Every request resolves to `default_country` — jurisdiction becomes a static
constant, which is only honest when the operator can genuinely assert
single-jurisdiction traffic. It is not only `granted` baselines that make
this dangerous: with a `requires_signal` baseline, a page-global CMP that
emits a consenting TCF string grants permissions for every mis-attributed
visitor just as effectively.

Constraint: **startup fails** when no geo provider is selected and any
**jurisdiction consumer** is enabled, unless the operator sets an explicit
acknowledgment (`[geo] assume_single_jurisdiction = true`). Jurisdiction
consumers are enumerated, not implied: an EC provider is selected,
server-side auction dispatch is gated on `regime` (§7), or any raw-EC /
EID egress path is active. An EC-provider-only exemption would be too
narrow — a stateless deployment still dispatches auctions off the policy's
regime class, and no geo + a permissive static jurisdiction misclassifies
EU traffic for that decision just as it would for identity. Only a
deployment with **no** jurisdiction-sensitive behavior is exempt. Without
this guard, the natural migration config (`default_country = "US"`, geo
unset) silently grants `store-on-device` and EID transmission to every EU
visitor — the highest-severity finding of the PR #838 review. The startup
log always prints the effective baseline and whether geo is live.

### 5.4 Defaults, two distinct fallbacks

`[geo] default_country` is required; startup fails without it (per #779).
It covers requests that resolve **no country at all**. Countries that
resolve but match no rule fall to the policy's `rules.default` entry
(§3.2). The two fallbacks are deliberately separate: "we could not place
this request" and "we placed it somewhere we have no rule for" are
different states, and pre-epic behavior treated them differently (fail
closed vs. non-regulated) — collapsing them is what made PR #838's
migration story unresolvable (migration spec §2, rows 5 and 7).

## 6. Failure-mode matrix — normative

| Condition                                            | Resolution behavior                                                                           |
| ---------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| Geo lookup fails at request time (provider selected) | `default_country` baseline                                                                    |
| No geo provider configured                           | `default_country` baseline, guarded by §5.3                                                   |
| Country resolved, no matching rule                   | Policy `rules.default`                                                                        |
| Region resolved, no region rule                      | Country rule                                                                                  |
| No `[permissions]` section                           | Compiled-in fallback: everything `requires_signal`, `regime = "gdpr"`                         |
| S2S sync request (no user signals)                   | Authorized by stored provenance re-validated against current policy (§7)                      |
| Malformed policy                                     | Rejected at config push / startup (§3.3) — never per request                                  |
| No `default_country`                                 | Startup failure                                                                               |
| Undecodable TCF/GPP record (present but malformed)   | Blocks grants (fail-closed acquisition, §4.4); never withdraws; opt-out signals still honored |
| Signals contradict (opt-out + consent)               | Opt-out wins (§4)                                                                             |

The intended posture is fail-closed, with its two exceptions stated rather
than glossed: geo lookup failure resolves to the configured default (§5.2's
declared, metered residual — permissive defaults make this path fail-open),
and the §5.3 static-jurisdiction configuration exists only behind an
explicit operator acknowledgment. Every other ambiguous state resolves to
the configured baseline or more restrictive.

## 7. Enforcement points

Consumers of the resolved set in this epic:

1. **EC provider execution** (providers spec §5) — the provider's declared
   `required_permissions()` must all be set for minting and identity use.
   This gate applies to EC providers only. **Geo** is ungated because
   gating it is circular — jurisdiction is an input to permission
   resolution. **Device** is ungated by a different, deliberate decision
   (it is _not_ a resolution input): its security-classification role must
   run for traffic that has granted nothing, and operator selection is the
   recorded authorization — providers spec §5 states the decision and its
   boundary. If a future vocabulary adds a purpose covering geolocation or
   fingerprinting, gating those providers will require a two-phase
   resolution specified then, not improvised.
2. **EC lifecycle** — creation requires `store-on-device`; withdrawal per
   §4.2. Recognition, canonicalization, and revocation of an existing
   identifier are **never** permission-gated — they must run precisely when
   permissions are withdrawn (providers spec §5).
3. **Every raw-EC egress and identity operation** — the concrete
   inventory, normative per path (one test per row; a denylist check
   proves no ungated egress exists):

   | Path                                                             | Required permissions                          | Notes                                                                                                                                        |
   | ---------------------------------------------------------------- | --------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
   | OpenRTB `user.id`                                                | `store-on-device` ∧ `select-personalised-ads` | Raw EC is identity in the bidstream — gated exactly as EIDs. PR #838 gated only EIDs, leaving `user.id` reachable with Purpose 4 refused     |
   | EC-derived auction request IDs                                   | both purposes                                 | Derived values are identity                                                                                                                  |
   | Page-bids path                                                   | both purposes                                 |                                                                                                                                              |
   | Bidstream EIDs                                                   | both purposes                                 | The one gate PR #838 had                                                                                                                     |
   | Proxy / click / Testlight forwarding of the EC cookie or headers | both purposes                                 | **New hardening, declared change** — these paths extract the raw cookie/header without today's jurisdiction gate (migration spec §2 row 11b) |
   | Identify endpoint (partner-facing)                               | both purposes                                 | Partner identity exchange, not a first-party lookup — decided here                                                                           |
   | Pull sync / batch sync (partner identity exchange)               | both purposes                                 | Authority source for S2S requests: stored provenance, below                                                                                  |
   | Request-scoped graph reads/writes (non-revocation)               | `store-on-device`                             |                                                                                                                                              |
   | Revocation paths (tombstones, withdrawal reads)                  | **exempt**                                    | Must work when permissions are unset                                                                                                         |
   | Stored consent-state lookup (§4.4)                               | **exempt**, narrowly scoped                   | Determining `store-on-device` cannot require `store-on-device`                                                                               |

   With **no EC provider configured**, identity use fails closed: a cookie
   value present on the request never egresses anywhere — never vacuously
   allowed (#838's `ec_allowed` was `is_none_or`, vacuously true with no
   provider).

   **S2S authority (batch/pull sync).** A server-to-server request carries
   no user signals, geo, or `EcContext` to resolve permissions from. Its
   authority is the identity's **stored provenance**: a record written at
   mint/update time carrying the resolved jurisdiction, regime, grant
   basis, and provider/version (providers spec §6.1). A sync request
   re-validates that provenance against the **current** policy revision:
   if the stored jurisdiction now resolves to `denied` for the required
   permission, the row is not updated and is flagged for the operational
   cleanup of §4.2 trigger 3. Sync never mints authority of its own.

4. **Server-side auction dispatch** — gated on the policy `regime` class,
   normatively:

   | Regime       | Dispatch rule                                                                                                                                                     | Preserves                 |
   | ------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------- |
   | `gdpr`       | Dispatch only with a decodable, unexpired TCF record consenting to Purpose 1. Malformed, expired, or absent record → **no bid request leaves** (no-bid response). | Today's GDPR/unknown arm  |
   | `us-privacy` | Dispatch proceeds in every signal state, including opt-out — the opt-out strips identity (rows above) but the contextual auction runs.                            | Today's US-state arm      |
   | `none`       | Dispatch proceeds.                                                                                                                                                | Today's non-regulated arm |

   The **compiled-in fallback policy has `regime = "gdpr"`** (§3.1) — the
   no-policy posture must be the most protective for dispatch too, and a
   regime-less fallback would leave dispatch undefined. When dispatch is
   blocked, nothing leaves for that request: no PBS/APS call, no UA/IP/geo
   forwarding to bidders. When dispatch proceeds, what the request may
   carry is governed row-by-row by the egress inventory; the full
   regulatory context (consent strings) is always forwarded so downstream
   partners make their own decisions (§1).

The client-cycle resolve endpoint (own spec, currently on hold) would be a
further consumer if and when it proceeds.

## 8. Testing strategy

- **The decision matrix is the test plan.** Every row of §4.1 × each
  baseline, every trigger of §4.2, and every row of §6, as table-driven
  tests. The ~24-case matrix deleted by PR #838 (net −18 tests in the
  consent module, replaced by happy-path cases only) is restored in
  equivalent form against the new API; signal-precedence conflicts
  (opt-out + consenting TCF) are mandatory cases, not optional ones.
- The §4.4 normalization matrix as table-driven tests, including every
  configured conflict mode and the malformed-record rows.
- The §7 raw-EC egress inventory: one test per inventoried egress proving
  the gate, plus a denylist-style check that no ungated egress exists.
- The §7 auction-dispatch matrix: every regime × signal state
  (consent, opt-out, malformed, expired, absent), including the
  no-policy fallback regime, asserting both the dispatch decision and
  that a blocked dispatch emits no outbound request.
- The §7 S2S authority path: sync against stored provenance, including
  the policy-tightened-to-denied case (no update, flagged for cleanup)
  and the exempt consent-state lookup.
- §4.3 fault-injection cases.
- Policy validation tests for every §3.3 rejection, exercised through both
  acceptance paths (push-time and startup).
- Shipped-table coverage test (§3.5) and jurisdiction-consistency test
  (§3.4) covering both legacy lists.
- One end-to-end integration scenario per posture: opt-in jurisdiction with
  and without consent, opt-out jurisdiction with GPC (including GPC + a
  consenting TCF string), the no-geo/default-country path, and the
  no-policy compiled fallback.

## 9. Out of scope

- Additional purposes (extension procedure in §2).
- A build-time-embedded policy file (PR #838's approach) — rejected for the
  reasons in §3.1, not deferred.
- Per-signal jurisdiction scoping (honoring GPC only where a law defines
  it): rejected in favor of the global rule in §4; revisiting it is a
  policy-model change requiring its own review.

## 10. Divergences from issue #779

This spec supersedes #779 on the following points; the issue is updated to
reference this spec when the PR merges, so there is one acceptance contract,
not two:

| #779 says                                                                                    | This spec says                                                                                                           | Why                                                                                                                                          |
| -------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------- |
| Unmatched countries fall to `default_country`                                                | Unmatched-but-resolved countries fall to the policy's `rules.default`; `default_country` covers only unresolved requests | The two states had different pre-epic behavior; collapsing them made migration unresolvable (§5.4)                                           |
| The full TCF purpose vocabulary is modeled                                                   | Only enforced purposes appear (§2)                                                                                       | Nine inert purposes in a policy file are a compliance hazard, not forward compatibility                                                      |
| Policy is an embedded file                                                                   | Policy is `[permissions]` in `trusted-server.toml` (§3.1)                                                                | Runtime config-store pipeline; validation at push time                                                                                       |
| Permission sources are open-ended (#777: publisher interaction, external services may grant) | Sources are jurisdiction, policy, and the §4 signal taxonomy; a pluggable source interface is **deferred** (§1)          | Shipping an interface with no second source repeats the inert-surface mistake; the extension path (a new §4 signal class) is defined instead |
