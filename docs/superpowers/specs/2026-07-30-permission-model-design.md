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
| `store-on-device`         | 1           | EC provider execution; EC creation; input to the §4.2 withdrawal decision (revocation itself is never permission-gated, §7)                                                                  |
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
# Illustrative schema example — NOT the shipped policy. The shipped
# example (trusted-server.example.toml) pairs these groups with the most
# protective rules.default; the permissive default below demonstrates the
# reserved key.
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
# US privacy gating applies per configured privacy state, matching today's
# state-list behavior; country-level US traffic (a Wyoming request, or one
# whose geo provider yields no region) stays non-regulated. One US/<state>
# rule per configured privacy state:
"US/CA" = "us-opt-out"
US = "non-regulated"
# Overrides name explicit acquisition rules — no +/- sigil syntax; TOML
# expresses the target state directly.
"US/CO" = { group = "us-opt-out", overrides = { select-personalised-ads = "requires_signal" } }
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
  the region part must be an assigned ISO 3166-2 subdivision of that country (not merely a shape check — `US/ZZ` would parse but can never match a request), unless the selected geo provider declares its own region vocabulary **together with a canonical mapping to ISO subdivisions** — §4.5 applicability and policy rule keys operate on canonical `US/CA`-form keys, so a provider emitting anything else must declare the translation, validated at startup. The `US/CA` slash form is the
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
  code; it accepts either a country (`FR`) or a country/region key whose
  region part is validated as an assigned subdivision exactly like rule
  keys (`US/ZZ` is rejected here too)
  (`US/CA`) — PR #838 supported region defaults, and a no-geo,
  single-state deployment must be able to select its state rule. It is
  canonicalized to uppercase, and startup logs which rule (or
  `rules.default`) it resolves to.

### 3.4 One source of jurisdiction truth

Today, `detect_jurisdiction` — driven by the runtime lists
`consent.gdpr.applies_in` and `consent.us_states.privacy_states` — is the sole
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
lists are in scope, not only the GDPR one — and the US check is
region-shaped: **every configured `consent.us_states.privacy_states` entry must
have a matching `US/<state>` rule**, and the country-level `US` rule must
resolve non-regulated (today applies privacy gating only to the configured
states), or the divergence is an explicit commented exception. An adapter
whose geo provider cannot resolve regions degrades **intentionally and
declaredly**: regionless US traffic hits the country rule — non-regulated,
today's behavior for non-privacy-state traffic; an operator preferring
protective country-wide gating writes `US = "us-opt-out"` as their own
declared choice.

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

- **Opt-out signals**, in two subclasses assigned by the §4.5 mapping:
  **destructive** opt-outs (GPC; sale opt-outs; USP opt-out) revoke and
  trigger withdrawal; **non-destructive** opt-outs (sharing,
  targeted-advertising) revoke the permissions they map to but never
  destroy the stored identity — a targeted-ads choice must not tombstone.
  Both subclasses are honored **globally**, not only in the jurisdictions whose law defines
  them — a deliberate, more-protective simplification: scoping a browser's
  explicit opt-out to a geolocation guess would honor it for some visitors
  and ignore it for others based on IP evidence. (For jurisdictions outside
  US states this is a declared behavior change; migration spec §2 records
  it.)
- **Grant signals** (affirmative permission): a decodable TCF record
  consenting to the purpose; an **explicit GPP non-opt-out value** (e.g.
  `sale_opt_out = false`); a **US Privacy string present and not opting
  out** — including the "not applicable" flag, which today's tests pin as
  allowing. Grant signals are what let a `requires_signal` US rule
  preserve today's "no signal → block, explicit non-opt-out → allow"
  behavior, which neither `granted` nor a TCF-only grant class could
  express. **Which grant evidence a rule accepts is regime- and
  permission-scoped** — grant signals are NOT interchangeable across
  regimes:

  | Regime of the resolved rule | Evidence accepted as a grant for a `requires_signal` permission                                                                                                                                                                       |
  | --------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | `gdpr`                      | **Only** a TCF record consenting to that specific purpose                                                                                                                                                                             |
  | `us-privacy`                | TCF consent for the purpose, or GPP/USP evidence **per the §4.5 field mapping** — a field grants only the permissions it maps to                                                                                                      |
  | `none`                      | TCF consent; GPP/USP grants only where §4.5 applicability yields one (the national section applies under `us-privacy` regimes, so in practice `none` grants via TCF — moot in the shipped policy, whose `none` baseline is `granted`) |

  Without this scoping, a US-style `sale_opt_out = false` would satisfy a
  French `requires_signal` rule — no TCF, both purposes granted, EC minted,
  partner egress authorized — contradicting the GDPR preservation row of
  the migration matrix. Auction dispatch blocking separately would not
  help; identity use would already be authorized. Opt-out signals and
  refusals remain regime-agnostic (global), as before: scoping applies
  only to what can _grant_, never to what can _revoke_.

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
4. Grant signal — a grant-class signal **accepted by the resolved rule's
   regime for that permission** (table above) grants it (subject to 1–3: a
   coexisting TCF refusal beats a non-TCF grant signal, matching today's
   US-state ordering where a present TCF record decides before GPP/USP
   values are consulted).
5. Malformed-present, no valid record of that family (§4.4) — **blocks
   the baseline grant**: the permission is unset even under `granted`.
   Never withdraws.
6. No signal — the policy baseline decides: `granted` sets it,
   `requires_signal` leaves it unset.

Normalization (§4.4) reduces each record family to exactly one of six
states — **valid-grant, valid-refusal, opt-out, malformed-present,
expired, absent** — and the precedence above plus the §4.1 matrix are
defined over those states, so no input state is unmapped.

### 4.1 Decision matrix

For each enforced permission, with baseline _B_ ∈ {granted,
requires_signal, denied}:

| Opt-out present | TCF refusal present | Accepted grant present (regime-scoped) | Malformed-present | Result                                           |
| --------------- | ------------------- | -------------------------------------- | ----------------- | ------------------------------------------------ |
| yes             | —                   | —                                      | —                 | **unset** (and withdrawal semantics apply, §4.2) |
| no              | yes                 | —                                      | —                 | unset (withdrawal per §4.2, trigger 2)           |
| no              | no                  | yes                                    | —                 | set, unless B = denied                           |
| no              | no                  | no                                     | yes               | **unset** (precedence 5 — blocks baseline grant) |
| no              | no                  | no                                     | no                | set iff B = granted                              |

### 4.2 Withdrawal vs. absence

Withdrawal (destructive: expire the EC cookie, write revocation tombstones)
and non-grant (the permission is simply unset) are distinct outcomes, never
conflated. "Baseline" below always means the **resolved acquisition rule for
`store-on-device` in the request's jurisdiction, after overrides** — never a
group label, since a group can mix rules across permissions.

The triggers, exhaustively — nothing else withdraws:

1. **A destructive opt-out signal (per §4.5's destructive column: GPC,
   sale opt-outs, USP opt-out) withdraws in every jurisdiction, whatever
   the baseline.** Non-destructive opt-outs (sharing,
   targeted-advertising) never trigger this — they revoke acquisition
   only. (For US states this preserves today's behavior; elsewhere it is
   the declared change of §4's global-opt-out rule.)
2. **A TCF record refusing `store-on-device` withdraws iff the baseline
   is `requires_signal` or `denied` — and only when the refusal is
   carried by the live request.** A persisted-KV consent record
   participates in acquisition only and **never triggers withdrawal**:
   without this, a refusal stored years ago under a `granted` policy
   would destructively fire on the first signal-less request after the
   policy tightens to `denied` — a policy edit tombstoning by proxy,
   which trigger 3 forbids, and the counterexample to §5.5's
   mixed-revision safety claim. Where the baseline is `granted`,
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

Withdrawal spans multiple KV writes and a cookie expiry; the contract for
partial failure is explicit (PR #838 expired the cookie first and
logged-and-swallowed tombstone-write failures, which can leave a live graph
identity with no browser handle pointing at it). The design centers on one
record that is simultaneously the durable intent, the discovery mechanism,
and the fail-closed marker:

- **The family revocation record is written first.** Every identity carries
  a stable **family ID**: minted rows store it, and — the case that makes
  or breaks the protocol — **rows that lack the field derive it
  deterministically** as a function of (record kind, provider namespace,
  canonical graph key), e.g. `fam:v0:hmac:<graph-key>`. Determinism is the
  point: a withdrawal arriving on the **first post-upgrade request** — a
  GPC-carrying visitor whose v1 row has no family field and has never been
  backfilled — computes the same family ID that every future reader of
  that row computes, so the revocation record is discoverable even if the
  writer crashes before ever touching the member row. A **random** ID
  would recreate the exact partial-withdrawal orphan this design exists to
  eliminate. Revocation writes one record keyed by the family ID; that
  single write is the withdrawal. Per-member tombstones are cleanup that
  follows, idempotent and retried.
- **Every consumer checks the family record, not per-member tombstones.**
  A reader arriving through any still-live member row finds the family ID
  in the row and the revocation record under it — partial revocation is
  discoverable from every member, and the record survives member-tombstone
  replacement (which today discards the original row's identity and
  metadata, making sibling discovery impossible).
- **Negative authority has its own permission-exempt record, with a
  complete transition contract.** A live refusal or opt-out must clear
  prior positive provenance, but the row write that would do it requires
  `store-on-device` — which the refusal just unset — and identity rows
  may be eventually consistent. The **suppression record**
  (`s`-class key per family, providers spec §6.3) resolves this. Its
  contract:

  **Creation is cause-aware and mostly read-free.** A live resolution
  whose outcome for a permission is unset writes suppression when the
  cause is a **signal state** — refusal, non-destructive opt-out,
  malformed-present — **unconditionally**, with no row read: conditioning
  on observing positive provenance through an eventually consistent row
  loses the race where a stale replica hides a just-committed grant. The
  one cause that inherently needs prior state — applicable **absence**
  clearing a previously positive permission — uses a narrow
  **permission-exempt suppression-decision read** exposing only the
  family ID and authority metadata (an undeclared exempt read was the
  alternative, and skipping it leaves stale S2S authority).
  **Policy-only tightening writes nothing**: a policy edit is not a user
  signal (§4.2 trigger 3), and a signal-less request after
  granted→denied must not create sticky user suppression that a policy
  rollback cannot undo.

  **Writes are CAS-fenced; evidence recency decides semantics.** The
  record requires linearizable per-key CAS (providers spec §7): CAS
  serialization prevents lost updates, but arrival order does **not**
  decide outcomes — each per-permission entry stores its cause, source
  class, and authoritative evidence timestamp (first-seen normalization
  for timestamp-less sources), and an incoming transition applies only
  when its evidence timestamp is **newer than or equal to** the stored
  entry's; ties resolve to the more restrictive state. So a delayed
  grant with `LastUpdated = 100` never clears a suppression whose
  refusal carried `200`, while a genuine re-consent at `300` does. The
  transition table, by stored cause: **opt-out from a timestamp-less
  source** — cleared only by a grant with an authoritative timestamp
  newer than the suppression's observation (sticky opt-out, sign-off
  16); **TCF refusal** — cleared by any regime-accepted grant with newer
  authoritative evidence; **malformed-present / absence** — cleared by
  any regime-accepted valid grant with newer evidence, including a
  timestamp-less grant whose first-seen is newer (these causes are not
  user opt-outs, so stickiness does not apply — without this rule, one
  truncated request would permanently deny a GPP-only user). Policy
  changes never clear user-signal suppressions.

  **Anti-replay for timestamps.** A future-dated record is rejected as
  malformed beyond the skew window; within it, the record's digest is
  stored with its **first normalized timestamp, which re-presentation
  never advances** — otherwise a future-dated TCF string replayed after
  an opt-out would keep re-normalizing to "now" and clear it. Equality
  digests are computed over the **canonical per-permission semantic
  result** of §4.5 aggregation, not the raw encoding — two encodings
  (or `N` vs explicit N/A) with the same meaning are the same evidence
  and keep the original first-seen, so alternating equivalent values
  cannot renew authority.

  **Boundary, retention, ordering.** Suppression is checked by **every
  `AuthorizedIdentity` constructor** (both scopes), by pull sync's live
  path, and by every S2S recompute — not only "partner egress" prose; a
  suppression read failure **fails closed** like a revocation read
  failure; retention must outlive the positive authority it masks
  (providers spec durability/retention capability); and **clearing is
  fenced on provenance visibility**: a clear entry records the
  provenance generation it reflects, and S2S honors the clear only when
  it can read that generation or newer — clearing first would expose the
  _older_ positive snapshot through an eventual read. **Write failure
  fails closed for the live request**, and the S2S residual is unbounded
  for a never-returning visitor (sign-off 11), with fault tests for
  suppress-vs-clear races, repeated-value sequences, and the
  stale-provenance-read case.

- **The cookie expires only after the family record commits.**
- **If the family-record write itself fails, nothing durable exists** —
  the cookie stays and the durable client-side signal (GPC, CMP-stored
  TCF) retries the whole withdrawal on the next request. Mitigations:
  while graph **writes are degraded** (health signal), S2S partner egress
  and sync updates fail closed on that instance (providers spec §6.2);
  the failure is logged at `error` with a metric feeding the operational
  repair path. The residual that remains — a single failed write on an
  otherwise healthy graph, for a visitor who **never returns** — is
  **unbounded**, not "bounded by return latency": return latency has no
  bound for a non-returning visitor, and the per-instance breaker does
  not reach other instances. Accepting this residual instead of building
  a durable external retry queue is **product sign-off item 11**
  (migration spec §8), not a footnote.
- **Consistency and retention are backend contracts with a single
  normative home**: the providers spec consistency matrix (§7). It — not
  this spec — states the requirement, and it requires a **strongly
  consistent (read-after-write) primitive** for revocation records; no
  bounded-lag alternative exists (an earlier draft here permitted one,
  which contradicted the matrix — an adapter with a two-second lag would
  have passed one spec and failed the other). A **failed family-record
  read fails closed** for egress (revoked-unknown ≠ live), and revocation
  records are retained beyond the maximum of cookie lifetime, row TTL,
  rewrite grace, and downstream retry horizon — today's 24-hour tombstone
  TTL is far below this bar and does not carry over.
- Fault-injection tests cover: family-record write fails → cookie
  untouched, S2S behavior per degraded mode, retry completes; member
  tombstone N fails after the family record → identity already revoked for
  every reader, cleanup retries; the same-signal retry path end to end;
  **first post-upgrade request is a withdrawal** (v1 row, no family field,
  derived ID; crash between family record and row write; reader of the
  untouched v1 row still sees the revocation).

### 4.4 Signal normalization — normative matrix

§4's precedence operates on normalized inputs: one effective consent
record and one effective opt-out state per request. The normalization
layer is where today's real-world mess lives, and PR #838 collapsed it
silently. These are the outcomes — decided here, not delegated to the
implementation; each row marked **changed** also appears in the migration
matrix. The pipeline order is itself normative: **(1) syntax validation
per source, (2) expiry per source — expired sources drop to absent
_before_ conflict resolution, (3) conflict resolution over the remaining
valid sources.** Current runtime resolves conflicts first and can select
an expired record before clearing both sources; expiry-first is a
**declared change** (migration matrix) that removes that path:

| Input state                                                      | Effective record / outcome                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | Status                                                                                                                            |
| ---------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| Standalone TCF and GPP-embedded TCF disagree, mode `restrictive` | **Whole-record selection comparing the P1 ∧ P4 conjunction only** — today's algorithm, preserved (an earlier draft's lexicographic (P1, P4) tuple would have changed split-purpose outcomes): if exactly one record's conjunction is false, `restrictive` selects it; **equal conjunctions — including split-purpose disagreements — keep the standalone record**, as current code does                                                                                                                                                                   | Preserved — pinned against current tests                                                                                          |
| Same, mode `permissive`                                          | Same conjunction comparison, selecting the record whose conjunction is true; equal conjunctions keep the standalone record                                                                                                                                                                                                                                                                                                                                                                                                                                | Preserved — same pinning                                                                                                          |
| Same, mode `newest`                                              | Whole-record selection by **`LastUpdated`** subject to the existing freshness threshold; a tie, an incomparable pair, or timestamps inside the threshold fall back to the `restrictive` rule above (itself deterministic)                                                                                                                                                                                                                                                                                                                                 | Preserved — same pinning                                                                                                          |
| Expired consent record                                           | Treated as **absent entirely** — grants nothing, refuses nothing, withdraws nothing; the baseline applies. Under a `granted` baseline that means the grant stands: an expired refusal is not current evidence and must not revoke indefinitely                                                                                                                                                                                                                                                                                                            | Preserved                                                                                                                         |
| One valid record + a second malformed record of the same family  | The **valid record governs**; the malformed one is ignored with a `warn` log. Fail-closed-on-malformed (below) applies only when no valid record of that family exists                                                                                                                                                                                                                                                                                                                                                                                    | Decided here                                                                                                                      |
| One valid record + one **expired** record of the same family     | The valid record governs — the expired one dropped at pipeline step 2, before conflict resolution ever saw it                                                                                                                                                                                                                                                                                                                                                                                                                                             | **Changed (declared)** — current runtime resolves the conflict first and can select the expired record                            |
| Persisted-KV consent record, live record present                 | **Live wins**, always; the stored record is never consulted                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | Preserved                                                                                                                         |
| Persisted-KV consent record, no live record                      | Substitutes as the effective record **iff within the same TTL as a live record**, then flows through the full normalization pipeline (syntax, expiry, conflict) like any live record; staler → absent. This narrow read is exempt from the graph-read permission gate (§7) — determining `store-on-device` cannot itself require `store-on-device`                                                                                                                                                                                                        | **Changed (declared)**: current code returns immediately after the KV load, bypassing expiry and conflict normalization           |
| Proxy/mirror mode                                                | **Minimal opt-out extraction still runs; full semantic decoding is skipped.** Because opt-outs are globally authoritative (§4), proxy mode must not suppress them: the §4.5-mapped opt-out fields (GPP US sections) and the US Privacy string are decoded — nothing else — alongside syntax validation, so a valid SaleOptOut or USP opt-out revokes and withdraws exactly as outside proxy mode. No grants are ever derived from records in proxy mode; a present record otherwise blocks grants (fail-closed); absent → baseline. GPC needs no decoding | **Changed (declared)**: today proxy mode skips decoding entirely — fail-open under permissive baselines and, worse, opt-out-blind |
| GPP / US Privacy fields                                          | Per the normative field mapping of §4.5 — fields are not interchangeable signals; **explicit N/A is grant-class (not-opted-out), absent grants nothing** — one meaning, everywhere                                                                                                                                                                                                                                                                                                                                                                        | Decided here (§4.5)                                                                                                               |
| Malformed-but-present record, no valid record of that family     | **Blocks grants** (fail-closed acquisition — it does not degrade to "absent", which under a `granted` baseline would turn garbage into a grant, the fail-open path in both #838 and the first draft of this spec). Never triggers withdrawal — destruction requires an affirmative, decodable signal (§4.2)                                                                                                                                                                                                                                               | Changed (declared)                                                                                                                |

### 4.5 US signal field mapping — normative

GPP and US Privacy fields map to specific permissions with specific
effects; they are never interchangeable, a field's absence or N/A value
behaves per its table row — **explicit _Not Applicable_ is grant-class
(not-opted-out), preserving current USP tests and GPP `NotApplicable`
handling; only a genuinely absent field contributes nothing** (this is
the single normative statement; an earlier "N/A contributes nothing"
rule is dead, and the P4-authorizing consequence is sign-off item 17) —
and only the fields marked destructive trigger
withdrawal. Section IDs and versions are those of the IAB GPP
specification current at implementation time; adding a section or field is
a change to this table.

| Source · field                               | Value                       | `store-on-device` (P1)                 | `select-personalised-ads` (P4)         | Destructive withdrawal?                                                                                            |
| -------------------------------------------- | --------------------------- | -------------------------------------- | -------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| GPP US section · `SaleOptOut`                | opted out                   | opt-out                                | opt-out                                | **Yes** (preserves today)                                                                                          |
| GPP US section · `SaleOptOut`                | not opted out               | grant                                  | grant                                  | —                                                                                                                  |
| GPP US section · `SharingOptOut`             | opted out                   | —                                      | opt-out                                | No                                                                                                                 |
| GPP US section · `SharingOptOut`             | not opted out               | —                                      | grant                                  | —                                                                                                                  |
| GPP US section · `TargetedAdvertisingOptOut` | opted out                   | —                                      | opt-out                                | **No** — a targeted-advertising choice must never destroy the stored identity                                      |
| GPP US section · `TargetedAdvertisingOptOut` | not opted out               | —                                      | grant                                  | —                                                                                                                  |
| US Privacy · `opt_out_sale`                  | `Y`                         | opt-out                                | opt-out                                | **Yes** (preserves today)                                                                                          |
| US Privacy · present, `N` or N/A             | —                           | grant                                  | grant                                  | — (today's tests pin N/A as allowing; USP carries no distinct targeted-advertising field, so it never maps to one) |
| Any field                                    | explicitly _Not Applicable_ | as the field's not-opted-out row above | as the field's not-opted-out row above | —                                                                                                                  |
| Any field                                    | absent                      | —                                      | —                                      | —                                                                                                                  |

**N/A vs absent (restating the single rule):** explicit _Not
Applicable_ = grant-class; absent = nothing; a non-applicable section's
fields grant nothing (their opt-outs still count, per step 2).

**Applicability and aggregation — ordered algorithm:**

1. **Section map (normative, pinned here — not "whatever GPP is
   current"), matching the official IAB registry in full:** section 6 ↔
   the **US Privacy string carried as a GPP section** — it maps to the USP
   rows of the field table in full, opt-outs _and_ grant-class values,
   under the same applicability rules as the national section; `US` national ↔ 7 (usnat);
   the state sections — `US/CA` ↔ 8, `US/VA` ↔ 9, `US/CO` ↔ 10,
   `US/UT` ↔ 11, `US/CT` ↔ 12, `US/FL` ↔ 13, `US/MT` ↔ 14, `US/OR` ↔ 15,
   `US/TX` ↔ 16, `US/DE` ↔ 17, `US/IA` ↔ 18, `US/NE` ↔ 19, `US/NH` ↔ 20,
   `US/NJ` ↔ 21, `US/TN` ↔ 22, `US/MN` ↔ 23, **`US/MD` ↔ 24,
   `US/IN` ↔ 25, `US/KY` ↔ 26, `US/RI` ↔ 27** (an earlier draft wrongly
   claimed MD/IN/KY/RI had no section). A truncated map silently loses
   opt-outs — a Texas (16) or Maryland (24) sale opt-out must not vanish.
   The implementation PR cross-checks this list against both the current
   decoder's section set and the official registry, and the accepted version per
   section is **pinned normatively to the named IAB GPP registry
   revision current at this spec's date (2026-08-01)** — "enumerated by
   the implementation PR" was two-implementations-diverge territory; a
   mapped section carrying a version outside the pinned revision is
   treated as malformed-present (blocks grants, never withdraws — §4.4),
   not as absent. Adding a section or version is a
   change to this map.
2. **Applicability gates grants only — never opt-outs.** A mapped
   **opt-out** field (either subclass) is honored from **any** section on
   **any** request, whatever the regime — this is §4's global-opt-out
   rule, and filtering it by jurisdiction would make a French visitor's
   `usnat SaleOptOut` simultaneously mandatory (§4) and ignored (here).
   For **grants**: the national section is applicable to any
   `us-privacy`-regime request; a state section is applicable iff it maps
   to the resolved `US/<state>`; foreign-state sections and all sections
   on non-`us-privacy` requests grant nothing. Regionless US traffic:
   national section only. A configured privacy state with no
   state-specific section uses the national section alone.
3. **State-over-national, per field — for grants only:** where an
   applicable state section carries a field, its value governs that
   field's **grant** derivation; the national section fills only fields
   the state section lacks. This precedence **never suppresses an
   opt-out**: a national-section opt-out stands even where the state
   section's same field says not-opted-out — step 2's global rule wins,
   or a state string could erase a globally authoritative national
   opt-out.
4. **Aggregate across what remains applicable:** an opt-out (of either
   subclass) in any applicable field beats a grant from another —
   restrictive aggregation.

`SharingOptOut` and `TargetedAdvertisingOptOut` are new enforcement
inputs — current code consults only the sale field — and are declared as
such in the migration matrix.

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

### 5.5 Policy revision activation

A policy edit propagates through the config store, so a fleet briefly
mixes revisions. The contract: instances stamp every resolution and every
provenance write with the policy revision they used (already required by
§7); the mixing window is bounded by config propagation and observable via
the config-version metric; and mixed-revision irreversibility is bounded and **accepted, not
denied** (sign-off 19): destructive withdrawal triggers are user
signals, never policy (§4.2 trigger 3) — the one revision-sensitive destructive case
(trigger 2 under a now-`denied` baseline) requires an affirmative user
refusal at the evaluating instance, which is safe under either revision.
S2S recomputation always evaluates against the instance's current
revision and records it. One divergence is explicitly accepted rather
than fenced: during convergence, a live refusal under a
`granted`-revision instance suppresses while the same refusal under a
tightened-revision instance destroys (trigger 2) — the destructive
outcome is the target revision's intended behavior arriving early on
part of the fleet, coordinated activation fencing is not worth its
machinery, and the acceptance is sign-off item 19. Rolling a policy
revision back restores acquisition rules but **cannot resurrect
tombstoned identities**; the migration guide says so where operators
will read it.

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

   | Path                                                             | Required permissions                                                   | Notes                                                                                                                                                                                                                                                                                                                                                                                             |
   | ---------------------------------------------------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
   | OpenRTB `user.id`                                                | `store-on-device` ∧ `select-personalised-ads`                          | Raw EC is identity in the bidstream — gated exactly as EIDs. PR #838 gated only EIDs, leaving `user.id` reachable with Purpose 4 refused                                                                                                                                                                                                                                                          |
   | EC-derived auction request IDs                                   | both purposes                                                          | Derived values are identity                                                                                                                                                                                                                                                                                                                                                                       |
   | Page-bids path                                                   | both purposes                                                          |                                                                                                                                                                                                                                                                                                                                                                                                   |
   | Bidstream EIDs                                                   | both purposes                                                          | The one gate PR #838 had                                                                                                                                                                                                                                                                                                                                                                          |
   | Proxy / click / Testlight forwarding of the EC cookie or headers | both purposes                                                          | **New hardening, declared change** — these paths extract the raw cookie/header without today's jurisdiction gate (migration spec §2 row 11b)                                                                                                                                                                                                                                                      |
   | Identify endpoint (partner-facing)                               | both purposes                                                          | Partner identity exchange, not a first-party lookup — decided here                                                                                                                                                                                                                                                                                                                                |
   | Pull sync (browser-request-scoped partner exchange)              | both purposes, from the **live** request resolution                    | Pull sync is created from a browser request and checks the live `EcContext` today — it keeps using the live P1 ∧ P4 decision plus the family revocation state (§4.3); stored provenance is never a substitute for available live evidence                                                                                                                                                         |
   | Batch sync (context-free S2S partner exchange)                   | both purposes, from **stored provenance**                              | The only truly signal-less path; authority rules below. Today's handler only authenticates and checks row state, so this gate is **declared hardening** (migration spec §2)                                                                                                                                                                                                                       |
   | Request-scoped graph reads/writes (non-revocation)               | `store-on-device`                                                      |                                                                                                                                                                                                                                                                                                                                                                                                   |
   | Revocation paths (tombstones, withdrawal reads)                  | **exempt**                                                             | Must work when permissions are unset                                                                                                                                                                                                                                                                                                                                                              |
   | Stored consent-state lookup (§4.4)                               | **exempt**, narrowly scoped                                            | Determining `store-on-device` cannot require `store-on-device`                                                                                                                                                                                                                                                                                                                                    |
   | Integration persistent response cookies                          | `store-on-device` (+ P4 where the cookie is an advertising identifier) | **Deferred with the hook's cookie surface** — the write-side gate alone was insufficient (read/use/forward/withdrawal unmodeled), so cookie operations ship only with the full model; this row and the client-cycle **page leg** (module injection gated on the provider's full declaration) join the inventory when their features do, and the §5.3 no-geo guard's consumer list grows with them |
   | Suppression-record writes (§4.3)                                 | **exempt**                                                             | Clearing authority is protective, like revocation                                                                                                                                                                                                                                                                                                                                                 |

   With **no EC provider configured**, identity use fails closed: a cookie
   value present on the request never egresses anywhere — never vacuously
   allowed (#838's `ec_allowed` was `is_none_or`, vacuously true with no
   provider).

   **S2S authority (batch sync).** A context-free server-to-server request
   carries no user signals, geo, or `EcContext`. Its authority is the
   identity's **stored provenance**: per-permission, time-bounded evidence
   written at mint and replaced on later live requests — grant basis
   (which signal class granted, per permission), the evidence's
   **authoritative timestamp and `valid_until`** (per evidence class),
   resolved jurisdiction, policy revision, and provider/version (providers
   spec §6.1). Freshness is a **per-evidence-class contract**, because not
   every source carries a timestamp:

   | Evidence class                                    | Authoritative timestamp                                                                                                                                                                                                                                                                                                | Age reset                                                                                                                    | Max age                   |
   | ------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- | ------------------------- |
   | TCF consent                                       | The record's `LastUpdated`                                                                                                                                                                                                                                                                                             | Only a record with a **newer** `LastUpdated`                                                                                 | Existing TCF expiry TTL   |
   | GPP / USP values (no intrinsic timestamp)         | **First-seen**: when TS first observed this exact normalized value (a **per-permission equality digest computed over only the applicable, aggregated §4.5 fields for that permission** — never the whole GPP record, or a CMP touching an unrelated notice field would mint a new digest and reset first-seen forever) | Re-presenting an identical digest **keeps the original first-seen**; a different value is new evidence with a new first-seen | Consent TTL (same as TCF) |
   | Policy-baseline grant (`granted` rule, no signal) | The policy revision that granted                                                                                                                                                                                                                                                                                       | Re-derived on every recompute against the current revision — policy is not user evidence and does not age; it changes        | n/a                       |

   Timestamps are compared with bounded clock-skew tolerance;
   beyond-window future-dated records are **rejected as malformed**, and
   within the window a record's first normalized timestamp is pinned to
   its digest and never advanced by re-presentation (§4.3's anti-replay
   rule — clamping every presentation to "now" would make a future-dated
   string perpetually fresh). And every live
   resolution **atomically replaces the complete per-permission
   snapshot**, never merges — a refusal, opt-out, malformed or absent
   state in the fresh resolution clears prior positive authority for its
   scope, so an old P4 grant cannot survive a later P4 refusal. A sync request performs a **full recompute of both
   permissions** from that stored evidence against the _current_ policy:
   it fails closed when the stored jurisdiction's rule is now `denied`,
   when a `granted` baseline tightened to `requires_signal` and the stored
   evidence contains no accepted grant for that permission, when the
   stored evidence has **expired**, or when the regime no longer accepts
   the stored grant's source class (§4's regime-scoped table). Any of
   these → no update, row flagged for the operational cleanup of §4.2
   trigger 3. Sync never mints authority of its own.

   **Legacy (pre-epic) rows** carry none of these fields. They are treated
   as reserved `hmac-v0` provenance with **no stored grant evidence**, so
   they **fail closed for partner egress and batch updates** until a live
   browser request lazily backfills provenance from a fresh resolution.
   Failing open here would grandfather every pre-epic identity past the
   permission model indefinitely.

4. **Server-side auction dispatch** — gated on the policy `regime` class,
   normatively:

   | Regime                                                                                                                                                                                          | Dispatch rule                                                                                                                                                                                                                                                                                                                                                                       | Preserves                                     |
   | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------- |
   | `gdpr`                                                                                                                                                                                          | Dispatch only with a decodable, unexpired TCF record consenting to Purpose 1. Malformed, expired, or absent record → **no bid request leaves** (no-bid response).                                                                                                                                                                                                                   | Today's GDPR/unknown arm                      |
   | `us-privacy`                                                                                                                                                                                    | Dispatch proceeds in every signal state, including opt-out — the opt-out strips identity (rows above) but the contextual auction runs.                                                                                                                                                                                                                                              | Today's US-state arm                          |
   | `none`                                                                                                                                                                                          | Dispatch proceeds.                                                                                                                                                                                                                                                                                                                                                                  | Today's non-regulated arm                     |
   | **Any regime, TCF-sourced effective record** — a raw TC string on the request, a GPP section-2 hint (both detected **before decoding**), or a persisted-KV fallback record of TCF origin (§4.4) | The `gdpr` row applies: dispatch requires the _effective_ record to be decodable, unexpired, and consenting to Purpose 1. A **malformed or expired** raw signal therefore blocks dispatch — today a malformed raw TCF blocks, and gating this arm on decodability would have silently relaxed that. A US or non-regulated request carrying a Purpose 1 refusal is likewise blocked. | Today's raw-signal arm — **must not regress** |

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
- The §7 S2S authority path: **every denial reason individually** —
  denied rule, tightened baseline without acceptable stored evidence,
  expired evidence, regime-rejected grant source — plus the exempt
  consent-state lookup, stale-evidence re-presentation (age must not
  reset), and legacy-row fail-closed-then-backfill.
- The full cross-product **regime × permission × evidence source** from
  §4's acceptance table and §4.5's field mapping, including multi-section
  aggregation conflicts and the applicability algorithm's foreign-section
  and regionless rows.
- Provenance snapshot-replacement transitions per permission: prior grant
  → refusal, → opt-out, → malformed, → absent — plus a mid-replacement
  fault proving the surviving state is the complete old **or** complete
  new snapshot, never a merged mixture.
- Legacy-row withdrawal end to end (§4.3's derived family ID).
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
