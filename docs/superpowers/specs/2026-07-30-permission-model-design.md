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

Scope: the model governs decisions Trusted Server makes. A downstream
protocol receives the full, unmodified regulatory context only when that
protocol normatively requires it and the destination is an authorized
privacy-signal consumer. Raw consent strings are request-scoped transport
data, not general identity metadata: ordinary identity rows retain the
normalized per-permission provenance and a digest, never the raw string
(§7; providers spec §6.3).

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
# example (`trusted-server.example.toml`) defines a `protective-default`
# group (`regime = "gdpr"`, `default = "requires_signal"`) and points
# `rules.default` to it; the permissive default below demonstrates the
# reserved key for the migration-preserving posture.
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

# Optional explicit entries live under one typed child map. Quoted permission
# IDs are required because they contain hyphens. An explicit entry replaces
# this group's default for only that permission.
[permissions.groups.us-opt-out.permissions]
"store-on-device" = "requires_signal"
"select-personalised-ads" = "requires_signal"

[permissions.rules]
FR = "gdpr-eu"
# US privacy gating has a protective country-wide floor. State rules may
# tighten or specialize it, but country-only geo and regionless US traffic
# never fall through to a non-regulated grant.
"US/CA" = "us-opt-out"
US = "us-opt-out"
# Overrides name explicit acquisition rules — no +/- sigil syntax; TOML
# expresses the target state directly.
"US/CO" = { group = "us-opt-out", overrides = { select-personalised-ads = "requires_signal" } }
# Reserved key: countries that resolve but match no rule. Required whenever
# the [permissions] section is present. Distinct from [geo] default_country,
# which handles requests that resolve no country at all (§5.4).
default = "non-regulated"
```

A group's `default` covers permissions absent from its optional
`permissions` child map. That map has the exact type
`BTreeMap<PermissionId, AcquisitionRule>`; direct permission-shaped keys on the
group object are unknown fields and rejected. An explicit map entry replaces
the group default for exactly that permission. A group without `default` must
list every vocabulary permission exactly once. Overrides map identifier → acquisition rule, so any
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
- group identifiers outside `[a-z0-9][a-z0-9-]{0,63}`; the lowercase ASCII
  grammar makes reference equality, JCS bytes, logs, and metrics identical on
  every runtime;
- groups that neither list every permission nor provide `default`;
- duplicate explicit permission entries, unknown permission IDs, a direct
  permission-shaped group key outside the `permissions` child map, or a value
  outside `granted | denied | requires_signal`; explicit entries take
  precedence over `default` and there is no merge-by-file-order behavior;
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
resolve to at least the `us-opt-out` protective floor; a more permissive
country fallback is invalid. An adapter whose geo provider cannot resolve
regions therefore degrades intentionally to country-wide US privacy gating,
never to `non-regulated`. A provider that can resolve a region still uses
the matching `US/<state>` rule first, and an operator may configure a
stricter country rule.

### 3.5 Shipped-table coverage

A CI test asserts every member of the GDPR country list resolves to a
GDPR-class baseline in the example policy. This closes a defect class
nothing in PR #838's validation covered: a mistyped country key (`DL:` for
`DK:`) parses cleanly, starts cleanly, and silently drops a member state to
the fallback rule. Countries intentionally unlisted are governed by the
`rules.default` entry (§3.2); the example policy documents that fallback
inline and ships the exact `protective-default` group described in §3.2
(`regime = "gdpr"`, every permission `requires_signal`). The separate
migration-preserving fixtures deliberately point `rules.default` to
`non-regulated` and declare that divergence (migration spec §5); the example
and migration fixture are not aliases.

## 4. Signal precedence — normative

Signals are classified into three classes — a two-class model (TCF grant /
opt-out) cannot reproduce today's US behavior, where no-signal traffic is
blocked but an **explicit non-opt-out** value grants:

- **Opt-out signals**, in two subclasses assigned by the §4.5 mapping:
  **use opt-outs** (GPC, sale, sharing, targeted-advertising, and USP sale
  opt-out) suppress the permissions they map to and persist negative authority
  but do **not** destroy the first-party identity merely because sale or
  sharing stopped; **destructive withdrawal signals** are limited to an
  explicit storage-consent withdrawal, an authenticated deletion request,
  or the TCF Purpose-1 refusal conditions of §4.2. Both subclasses are
  honored **globally**, not only in the jurisdiction whose law defines them —
  scoping an explicit choice to a geolocation guess would honor it for some
  visitors and ignore it for others. “Global” follows the identity's use;
  it does not broaden a sale/sharing choice into deletion.
- **Grant signals** (affirmative permission): a decodable TCF record
  consenting to the purpose; an **explicit GPP non-opt-out value** (e.g.
  `sale_opt_out = false`); a **US Privacy string present with an explicit
  `N` value**. _Not Applicable_, missing, reserved, unknown, and unsupported
  values never grant. Grant signals are what let a `requires_signal` US rule
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
  only to what can _grant_, never to what can _suppress_.

- **Refusals**: a decodable TCF record refusing the purpose. A refusal is
  neither a grant nor an opt-out — it blocks acquisition (precedence 3)
  and withdraws only per §4.2.

**Precedence, highest first:**

1. Policy `denied` — never set, regardless of any signal.
2. **Opt-out signal — always suppresses its mapped use**, regardless of any
   consent record present. A GPC header suppresses
   `select-personalised-ads` even when an accompanying TCF string consents to
   it; it does not itself revoke `store-on-device` or request deletion.
   _(This is the rule PR #838 inverted: its resolution returned from
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

| Opt-out present | TCF refusal present | Accepted grant present (regime-scoped) | Malformed-present | Result                                                                                                                                                                                                                                                                                                                                      |
| --------------- | ------------------- | -------------------------------------- | ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| yes             | —                   | —                                      | —                 | **unset** (and withdrawal semantics apply, §4.2)                                                                                                                                                                                                                                                                                            |
| no              | yes                 | —                                      | —                 | unset (withdrawal per §4.2, trigger 2)                                                                                                                                                                                                                                                                                                      |
| no              | no                  | yes                                    | —                 | set, unless B = denied                                                                                                                                                                                                                                                                                                                      |
| no              | no                  | no                                     | yes               | **unset** (precedence 5 — blocks baseline grant)                                                                                                                                                                                                                                                                                            |
| no              | no                  | no                                     | no                | set iff B = granted **and no suppression entry stands** — an active suppression (§4.3) beats the baseline, so one malformed request denies later no-signal requests under `granted` until newer valid grant evidence clears it; a policy-baseline grant alone does **not** clear non-user suppression (sign-off 24 covers this consequence) |

### 4.2 Withdrawal vs. absence

Withdrawal (destructive: expire the EC cookie, write revocation tombstones)
and non-grant (the permission is simply unset) are distinct outcomes, never
conflated. "Baseline" below always means the **resolved acquisition rule for
`store-on-device` in the request's jurisdiction, after overrides** — never a
group label, since a group can mix rules across permissions.

The triggers, exhaustively — nothing else withdraws:

1. **An explicit storage withdrawal or authenticated deletion request
   withdraws in every jurisdiction, whatever the baseline.** GPC and
   sale/sharing/targeted-advertising opt-outs are use restrictions, not
   deletion requests: they persist suppression for their mapped permissions
   but never trigger family revocation by themselves.
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

Withdrawal checking follows §4 precedence: a destructive signal from the
exhaustive list above triggers withdrawal even when other evidence grants;
a use opt-out suppresses only its mapped use and never enters this path.
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
  canonical graph key), per the providers spec §6.3 derivation (`tsfam1|` + record-kind byte + provider code + graph key). Determinism is the
  point: an explicit storage withdrawal arriving on the **first
  post-upgrade request** — from a visitor whose v1 row has no family field and has never been
  backfilled — computes the same family ID that every future reader of
  that row computes, so the revocation record is discoverable even if the
  writer crashes before ever touching the member row — and the write is
  admitted through the **observed-row sequence** (providers spec §5:
  successful row read → create-if-absent stub with no positive
  authority → family revocation → use denied in between), since an
  untouched v1 row has no authority-state record for the plain
  admission arm to find. A **random** ID
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
  complete transition contract.** A non-destructive refusal or use opt-out must clear
  prior positive provenance, but the row write that would do it requires
  `store-on-device` — which the refusal just unset — and identity rows
  may be eventually consistent. The **suppression record**
  (`s`-class key per family, providers spec §6.3) resolves this. Its
  contract:

  **Creation is cause-aware and mostly read-free.** A live resolution
  whose outcome for a permission is unset writes suppression when the
  cause is a **signal state** — a refusal that is not destructive under
  §4.2, a use opt-out, or malformed-present — **unconditionally** —
  meaning independent of _prior positive authority_, never independent of **family admission** (every durable write still passes the providers spec §5 admission arms; for an observed v1 row the non-destructive sequence applies) — with no row read needed for the decision itself: conditioning
  on observing positive provenance through an eventually consistent row
  loses the race where a stale replica hides a just-committed grant. The
  old rowless identity is the explicit boundary: its live refusal,
  malformed-present state, or use opt-out still denies this request but creates
  no `s`, `q`, `fam`, or `w` record. If P1 permits ordinary same-request
  re-minting, the new graph-backed family's authority-state commit includes
  that live suppression (or its negative intent) before the new cookie or
  identity is usable; otherwise no durable state exists and the next
  presentation is reevaluated. The `w` class remains destructive-withdrawal
  only. The
  one cause that inherently needs prior state — applicable **absence**
  clearing a previously positive permission — uses a narrow
  **permission-exempt suppression-decision read** exposing only the
  family ID and authority metadata (an undeclared exempt read was the
  alternative, and skipping it leaves stale S2S authority). Absence is
  **one-shot per positive summary**: writing the entry also retires the
  summary that justified it (recorded as retired-at), so later
  signal-less requests find no positive authority and write nothing —
  otherwise each would re-observe and extend the denial forever — and
  the entry's `valid_until` is capped by the retired authority's own
  original `valid_until` (absence retires an authority horizon; it does
  not outlive it).
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
  refusal carried `200`, while a genuine re-consent at `300` does.
  **Suppression expiry is cause-specific.** Refusal, malformed, and absence
  entries carry `valid_until` derived from the evidence or retired-authority
  horizon and become inert at expiry. A valid use opt-out (GPC,
  sale/sharing/targeted-advertising, or USP sale opt-out) is different:
  it remains effective until a strictly newer, explicit, regime-accepted
  opt-in/authorization clears it, or until deletion of the identity makes
  the record unnecessary. Passage of a consent TTL alone never restores
  sale/sharing or personalized-ad use. **"Strictly newer" requires an
  authoritative order, not later receipt:** either a regime-accepted TCF
  grant whose valid `LastUpdated` is after the opt-out evidence, or an
  authenticated same-subject authorization action that commits a monotonic
  authorization revision in the strong authority record. A bare GPP/USP
  not-opted-out value has no authoritative timestamp and therefore cannot by
  itself clear a persisted use opt-out; treating its new receipt time as
  recency would let replay of an older string restore processing. This epic
  does not invent an authenticated authorization endpoint: until a separate
  approved flow supplies that revision, only qualifying authoritative TCF
  evidence or identity deletion can clear such a suppression. The
  transition table (causes without an intrinsic timestamp — malformed
  records decode no `LastUpdated`, absence has no source — use their
  **observation timestamp**, server receipt on the shared clock basis
  within the skew window; cross-source comparison uses the authoritative
  timestamp where one exists, else the observation timestamp, ties
  restrictive), by stored cause: **opt-out from a timestamp-less
  source** — cleared only by the ordered explicit authorization defined
  above; an
  exact semantic replay keeps its original first-seen and cannot clear or
  refresh authority age. **A currently presented restrictive value still
  starts a new restrictive episode after an ordered clear**: a timestamp-less
  source cannot prove that its presentation predates the authenticated clear,
  so the privacy-protective result is a new suppression transition whose
  clearing floor is the current `authorization_revision`, while the evidence's
  original first-seen remains unchanged. A later clear therefore needs another
  authenticated monotonic increment (or qualifying newer TCF evidence).
  Restrictive evidence never clears positive or negative state. Replay-history
  saturation never shortens a newly
  observed restrictive choice: grant-class history is evicted before
  restrictive history, and a restrictive overflow updates the bounded
  per-permission restrictive marker to preserve at least the full
  opt-out horizon. **TCF refusal** — cleared by any regime-accepted grant with newer
  authoritative evidence; **malformed-present / absence** — cleared by
  any regime-accepted valid grant with newer evidence, including a
  timestamp-less grant whose first-seen is newer, **and — recovery
  observation, scoped to these two causes only — by a regime-accepted
  valid grant whose _presentation_ is observed after the suppression's
  observation timestamp, even when its pinned first-seen is older**:
  the common recovery is the unchanged grant re-presented after one
  truncated request, whose first-seen predates the malformed event by
  construction — first-seen comparison alone would deny until the
  suppression's TTL. The scope is what keeps replay harmless: clearing
  a malformed/absence suppression asserts only "the CMP currently
  emits valid state", which any valid presentation proves; the grant's
  own pinned first-seen and expiry are untouched (no age refresh), and
  opt-out causes still clear only on strictly newer authoritative
  evidence (these causes are not user opt-outs, so stickiness does not
  apply — without recovery, one truncated request would deny a
  GPP-only user for the suppression's full TTL). Policy
  changes never clear user-signal suppressions, and administrative repair
  may delete a suppression only with an auditable record of the consumer's
  newer authorization or deletion request.

  **Anti-replay for timestamps.** A future-dated record is rejected as
  malformed beyond the skew window; within it, the record's digest is
  stored with its **first normalized timestamp, which re-presentation
  never advances** — otherwise a future-dated TCF string replayed after
  an opt-out would keep re-normalizing to "now" and clear it. Equality is
  **source-specific**: for GPP/USP the digest is the **canonical
  per-permission semantic result** of §4.5 aggregation alone — two
  encodings of the same explicit applicable value are the same
  evidence and keep the original first-seen, so alternating equivalent
  values cannot renew authority; for TCF the digest is the semantic
  result **plus the authoritative `LastUpdated`** — a genuine CMP
  renewal with unchanged purposes carries a newer `LastUpdated` and
  legitimately refreshes authority age, which a semantics-only digest
  would wrongly ignore.

  **Boundary, retention, ordering — without deadlocking recovery.**
  Suppression is checked by **every `AuthorizedIdentity` constructor**
  (both scopes), by pull sync's live path, and by every S2S recompute.
  That gate plus clear-after-provenance would deadlock re-consent —
  fresh P1 provenance cannot be written while P1 suppression blocks
  `GraphOps`, and clearing first is forbidden — so recovery has its own
  narrow write path: **`AuthorityRefresh`**, permission-exempt but
  strictly scoped to committing provenance from the _current live
  resolution_ — its exact access set: **read + generation-CAS of the row
  being refreshed, and read + CAS of the family's authority-state
  record** (its own clearing protocol requires both; the earlier "no
  reads beyond the row" wording forbade a read its own CAS needs);
  nothing else — no partner writes, no egress — while suppression
  remains effective;
  the clear then references that provenance's revision. Revisions are an
  **application-level monotonic counter written with the row** — never
  backend generation markers, which (per Fastly's own contract) only
  detect change and carry no order — and S2S honors a clear only when it
  can read that revision or newer; clearing first would expose the
  _older_ positive snapshot through an eventual read.

  **The strong record carries positive-authority state too.** The
  per-family record doubles as the **authority-state record**: alongside
  negative entries it stores a per-permission positive-authority summary
  — **kind** (user evidence vs. policy-baseline), grant basis / source
  class, policy revision, `valid_until`, provenance revision, and
  evidence timestamp — CAS-updated by every provenance write. The kind
  and policy revision are load-bearing: the absence decision must
  distinguish vanished _user_ evidence (suppress) from a
  policy-baseline grant that disappeared because the _policy_ changed
  (never suppress — trigger 3), and a revision-and-timestamp-only
  summary would force exactly the eventual row read this record exists
  to eliminate.
  The **absence decision reads this strong summary, never the eventual
  identity row** — deciding "no prior authority" from an eventual
  not-found loses the race where a just-committed grant is invisible on
  a stale replica. A suppression/authority read failure **fails closed**
  like a revocation read failure. Every positive identity decision fresh-reads
  family revocation, authority/suppression, pending outbox, applicable `w`,
  and the global breaker; successful absence/health/authority has no lease.
  Only a typed restrictive result may be cached, and only to deny. Retention must outlive the positive
  authority it masks (providers spec durability/retention capability).

  **The strong record is the commit point — the two-record protocol is
  explicit.** Every provenance-bearing write spans the eventual identity
  row and the strong authority-state record, in a fixed order with
  defined intermediate states: (1) the row commits at revision _r_
  (generation-CAS); (2) the authority-state record CAS-updates its
  summary to _r_ — and that transition **rejects regression**: an
  incoming revision lower than the stored one is refused (a delayed
  commit for r2 arriving after r3's must not restore older authority or
  an older `valid_until`), and an equal revision is idempotent and must
  be payload-equivalent (a mismatch at equal revision is a hard error,
  not a merge). The r2-row → r3-row → r3-authority → delayed
  r2-authority schedule is a named test. **Revision _r_ is committed — usable by S2S, visible to the absence
  decision — only when the strong record reports it, and identity/S2S
  use requires `row.provenance_revision == authority.summary_revision`,
  failing closed in _both_ mismatch directions** (row r2 + summary r1 is
  the ordinary uncommitted case; summary r2 + eventually-stale row r1 is
  the inverse another region can strongly read — so the recompute takes
  **every** input, jurisdiction included, from the strong summary, never
  the row, and refuses on any revision mismatch, closing the
  wrong-jurisdiction egress). A
  row at _r_ whose summary still reads _r−1_ is simply uncommitted
  detail, and a crash between the writes leaves a recoverable state (the
  next live resolution re-runs step 2 via `AuthorityRefresh`), never a
  divergent one. This ordering is why the absence decision can trust the
  summary: there is no state in which the row authorizes something the
  strong record has never heard of. Minting follows the same rule —
  see the providers spec §5 order, where eligibility begins at the
  **authority-state commit**, not the row commit.

  **Write failure fails closed beyond the live request.** A deployment that
  enables persisted identity use must also provide a durable
  negative-intent outbox, independent of both the identity row's eventual
  store **and the strong target record's failure domain**.
  If a family-revocation or suppression CAS fails, the same request durably
  enqueues the idempotent negative intent before it can complete; workers
  retry it until the strong record commits. Every live, cached, and S2S
  identity decision checks the per-family outbox before positive use; a
  pending revocation denies the family and a pending suppression denies its
  mapped permission. If neither the target record nor the outbox can commit,
  a globally visible identity safety breaker disables all positive identity
  mint, use, graph access, and egress until repair; negative repair,
  withdrawal, and authenticated deletion paths remain enabled. An adapter
  that provides none of these primitives is ineligible for stateful identity.
  “Independent” is a qualification result, not a second key prefix in one
  store: target and outbox use distinct durability/failure domains. The
  breaker may share the outbox domain only when that domain proves this
  failure contract: if it cannot accept either the family enqueue or the
  breaker CAS, every subsequent strong outbox/breaker read fails rather than
  returning a stale successful absence. Every positive decision performs
  those reads fresh; no success lease is allowed. Implementations may cache
  only a typed restrictive result — revoked, suppressed, pending, or
  breaker-tripped — and may use that cache only to deny. A cached restrictive
  result cannot construct an `AuthorizedIdentity`, clear or acknowledge state,
  or drive a CAS; stale denial may reduce availability after recovery but can
  never authorize use. Absence, health, and positive authority are never
  cached for a positive decision. Under the qualified fault
  model, target failure leaves outbox/breaker available, while outbox-domain
  failure leaves either the target committed or all positive readers closed.
  An adapter that cannot prove those outcomes is ineligible even if all three
  APIs individually advertise CAS.

  The outbox has one total state machine. Each family record carries
  `schema_version`, a monotonic `queue_revision`, and a bounded map of pending
  negative transitions keyed by the deterministic §6.3 provider-wire
  `intent_id`. That wire schema is the sole definition of the materialized JCS
  transition payload: cause, source class, evidence time and digest, permission,
  state, validity, and the clearing-floor authorization revision all
  participate in identity; enqueue time and queue metadata do not. A producer
  that cannot construct the complete payload cannot enqueue a transition and
  must commit the global breaker. Only family revocation and
  creation/strengthening of suppression
  enter the map; a failed clear is never queued as negative intent because the
  existing denial is already safe. Enqueue CAS-unions entries: family
  revocation is absorbing; per-permission conflicts use the authority-state
  transition comparator (newer authoritative evidence, restrictive on a tie),
  and an older arrival cannot replace newer negative evidence. The cap is 32
  pending intents per family. Revocation consumes one slot and suppression
  entries consume one per permission/source; exact duplicates consume none.
  Capacity overflow must commit the global breaker before returning and cannot
  evict a negative intent.

  A worker applies the exact transition idempotently to the target, then
  CAS-removes that `intent_id` only if the queue revision and stored bytes still
  match. Target success followed by acknowledgment failure leaves a harmless
  pending denial and retry; acknowledgment can never precede target commit.
  Empty records are deleted by CAS. Queue retention is the maximum horizon of
  all contained transitions plus the recovery/audit window and can never be
  shorter than the target negative state. Unknown schema, malformed intent,
  read error, revision regression, or retention uncertainty denies the family
  rather than skipping the queue.
  Fault tests cover
  suppress-vs-clear races, enqueue/enqueue merge, target-success/ack-failure,
  stale-worker acknowledgment, capacity overflow, outbox replay,
  target-domain outage, outbox-domain outage, breaker propagation, and the
  stale-provenance-read case.

- **The cookie expires only after the family record commits.**
- **If the family-record write itself fails, negative intent still becomes
  durable before browser state changes.** The cookie stays; the writer
  enqueues the family-scoped intent in the required outbox and retries the
  family record asynchronously. A failed outbox enqueue trips the global
  identity safety breaker; a per-instance health flag is insufficient because a
  different instance could otherwise continue partner use for a visitor who
  never returns. The error and breaker state are logged and metered, and the
  breaker clears only after the outbox and strong record are healthy, the
  queue is drained through its recovery watermark, and an audit event records
  the controller action.
- **Consistency and retention are backend contracts with a single
  normative home**: the providers spec consistency matrix (§7). It — not
  this spec — states the requirement, and it requires **globally observable
  strong consistency** for revocation records — every instance's read
  observes a committed revocation, never merely the writer's own
  session (this spec deliberately repeats the provider contract's exact
  wording rather than paraphrasing it into the weaker "read-after-write");
  no bounded-lag alternative exists (an earlier draft here permitted one,
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
  **first post-upgrade request is an explicit storage withdrawal** (v1 row, no family field,
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

| Input state                                                      | Effective record / outcome                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        | Status                                                                                                                            |
| ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| Standalone TCF and GPP-embedded TCF disagree, mode `restrictive` | **Whole-record selection comparing the P1 ∧ P4 conjunction only** — today's algorithm, preserved (an earlier draft's lexicographic (P1, P4) tuple would have changed split-purpose outcomes): if exactly one record's conjunction is false, `restrictive` selects it; **equal conjunctions — including split-purpose disagreements — keep the standalone record**, as current code does                                                                                                                                                           | Preserved — pinned against current tests                                                                                          |
| Same, mode `permissive`                                          | Same conjunction comparison, selecting the record whose conjunction is true; equal conjunctions keep the standalone record                                                                                                                                                                                                                                                                                                                                                                                                                        | Preserved — same pinning                                                                                                          |
| Same, mode `newest`                                              | Whole-record selection by **`LastUpdated`** subject to the existing freshness threshold; a tie, an incomparable pair, or timestamps inside the threshold fall back to the `restrictive` rule above (itself deterministic)                                                                                                                                                                                                                                                                                                                         | Preserved — same pinning                                                                                                          |
| Expired consent record                                           | Treated as **absent entirely** — grants nothing, refuses nothing, withdraws nothing; the baseline applies. Under a `granted` baseline that means the grant stands: an expired refusal is not current evidence and must not revoke indefinitely                                                                                                                                                                                                                                                                                                    | Preserved                                                                                                                         |
| One valid record + a second malformed record of the same family  | For standalone-vs-embedded **TCF**, the valid record governs and the malformed one is ignored with a `warn` log. This row does not govern GPP's independently parsed multi-section aggregation; §4.5's mapped-section blocker does                                                                                                                                                                                                                                                                                                                | Decided here                                                                                                                      |
| One valid record + one **expired** record of the same family     | The valid record governs — the expired one dropped at pipeline step 2, before conflict resolution ever saw it                                                                                                                                                                                                                                                                                                                                                                                                                                     | **Changed (declared)** — current runtime resolves the conflict first and can select the expired record                            |
| **Expired** live record + still-valid persisted-KV record        | The expired live record is absent entirely (step 2), so it does **not** suppress the fallback: the persisted record substitutes, subject to its own TTL and the full pipeline — "live wins" applies to live records that still exist after expiry filtering                                                                                                                                                                                                                                                                                       | Decided here                                                                                                                      |
| Persisted-KV consent record, live record present                 | **Live wins**, always; the stored record is never consulted                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Preserved                                                                                                                         |
| Persisted-KV consent record, no live record                      | Substitutes as the effective record **iff within the same TTL as a live record**, then flows through the full normalization pipeline (syntax, expiry, conflict) like any live record; staler → absent. This narrow read is exempt from the graph-read permission gate (§7) — determining `store-on-device` cannot itself require `store-on-device`                                                                                                                                                                                                | **Changed (declared)**: current code returns immediately after the KV load, bypassing expiry and conflict normalization           |
| Proxy/mirror mode                                                | **Minimal opt-out extraction still runs; full semantic decoding is skipped.** Because opt-outs are globally authoritative (§4), proxy mode must not suppress them: the §4.5-mapped opt-out fields (GPP US sections) and the US Privacy string are decoded — nothing else — alongside syntax validation, so a valid SaleOptOut or USP opt-out suppresses P4 exactly as outside proxy mode. No grants are ever derived from records in proxy mode; a present record otherwise blocks grants (fail-closed); absent → baseline. GPC needs no decoding | **Changed (declared)**: today proxy mode skips decoding entirely — fail-open under permissive baselines and, worse, opt-out-blind |
| GPP / US Privacy fields                                          | Per the normative field mapping of §4.5 — fields are not interchangeable signals; **explicit N/A and absence both grant nothing**; only an explicit applicable not-opted-out value can grant                                                                                                                                                                                                                                                                                                                                                      | Decided here (§4.5)                                                                                                               |
| Malformed-but-present record, no valid record of that family     | **Blocks grants** (fail-closed acquisition — it does not degrade to "absent", which under a `granted` baseline would turn garbage into a grant, the fail-open path in both #838 and the first draft of this spec). Never triggers withdrawal — destruction requires an affirmative, decodable signal (§4.2)                                                                                                                                                                                                                                       | Changed (declared)                                                                                                                |

### 4.5 US signal field mapping — normative

GPP and US Privacy fields map to specific permissions with specific
effects; they are never interchangeable. A field's absence, explicit
_Not Applicable_ value, reserved value, unknown value, or unsupported
version contributes nothing and can never authorize processing. Only an
explicit applicable “did not opt out” value is grant-class, and no
sale/sharing/targeted-advertising field is destructive. Section IDs and
versions are those of the IAB GPP
specification pinned by the vendored snapshot; adding a section or field is
a change to this table.

| Source · field                               | Value                       | `store-on-device` (P1) | `select-personalised-ads` (P4) | Destructive withdrawal? |
| -------------------------------------------- | --------------------------- | ---------------------- | ------------------------------ | ----------------------- |
| GPP US section · `SaleOptOut`                | opted out                   | —                      | opt-out                        | No                      |
| GPP US section · `SaleOptOut`                | not opted out               | —                      | grant                          | —                       |
| GPP US section · `SharingOptOut`             | opted out                   | —                      | opt-out                        | No                      |
| GPP US section · `SharingOptOut`             | not opted out               | —                      | grant                          | —                       |
| GPP US section · `TargetedAdvertisingOptOut` | opted out                   | —                      | opt-out                        | No                      |
| GPP US section · `TargetedAdvertisingOptOut` | not opted out               | —                      | grant                          | —                       |
| US Privacy · `opt_out_sale`                  | `Y`                         | —                      | opt-out                        | No                      |
| US Privacy · `opt_out_sale`                  | `N`                         | —                      | grant                          | —                       |
| Any field                                    | explicitly _Not Applicable_ | —                      | —                              | —                       |
| Any field                                    | absent / unknown / reserved | —                      | —                              | —                       |

**N/A vs explicit non-opt-out:** _Not Applicable_ is not affirmative
permission and contributes nothing. An explicit applicable not-opted-out
value can grant only the permission mapped by its row and only under the
regime/applicability rules below.

**Unknown section IDs contribute nothing — and bound what
embedded-GPC scanning can promise.** A section ID outside the pinned map
is ignored (its fields neither grant nor revoke; known sections in the
same string remain valid — an unknown _section_ is not a malformed
_family_, unlike a mapped section at an unpinned version). Consequence,
stated honestly: an embedded GPC bit inside an unknown section is
undetectable by a decoder that cannot parse it — global-GPC coverage is
bounded by the pinned map's currency, which is one reason snapshot
updates are reviewed spec changes. Mixed known/unknown strings resolve
per-section by these rules.

**Embedded GPC is mapped, not ignored.** The US sections carry
`GpcSegmentIncluded` and `Gpc` fields; a request with embedded
`Gpc = true` and no `Sec-GPC` header was previously unspecified despite
the global-GPC rule. Normatively: embedded `Gpc = true` in **any**
section is the same **global P4 use opt-out** as the header (aggregated
with it by OR — opt-outs are never jurisdiction-filtered);
`GpcSegmentIncluded = false`, an absent segment, or `Gpc = false`
contributes nothing; a malformed optional GPC segment renders that
section malformed-present (blocks grants, never withdraws).

**Applicability and aggregation — ordered algorithm:**

1. **Section map (normative, pinned here — not "whatever GPP is
   current"), matching the official IAB registry in full:** section 6 ↔
   the **US Privacy string carried as a GPP section** — it maps to the USP
   rows of the field table in full, opt-outs _and_ grant-class values,
   under the same applicability rules as the national section; `US` national ↔ 7 (usnat);
   the state sections — `US/CA` ↔ 8, `US/VA` ↔ 9, `US/CO` ↔ 10,
   `US/UT` ↔ 11, `US/CT` ↔ 12, `US/FL` ↔ 13, `US/MT` ↔ 14, `US/OR` ↔ 15,
   `US/TX` ↔ 16, `US/DE` ↔ 17, `US/IA` ↔ 18, `US/NE` ↔ 19, `US/NH` ↔ 20,
   `US/NJ` ↔ 21, `US/TN` ↔ 22, `US/MN` ↔ 23, `US/MD` ↔ 24,
   `US/IN` ↔ 25, `US/KY` ↔ 26, and `US/RI` ↔ 27. All accepted versions
   and layouts are pinned by the inline GPP registry snapshot in §4.5.1; the
   current snapshot accepts version 1 for sections 24–27 from the official IAB
   registry commit named there. A truncated map silently loses opt-outs,
   so every accepted section is an implementation prerequisite rather
   than an inert placeholder. **The current decoder is an explicit
   prerequisite gap**: it (and `iab_gpp` 0.1.2) does not implement the
   complete pinned set and models `usnat` v2 while the snapshot pins v1.
   Implementation must add the missing official layouts and reject
   versions the library happens to decode but the snapshot disallows.
   The implementation PR cross-checks this list against both the current
   decoder's section set and the official registry, and the accepted version per
   section is **pinned to the vendored registry snapshot in §4.5.1** — the
   inline table enumerates, per mapped section, the
   accepted version(s), taken from the IAB registry at ratification (a
   date is not an immutable identifier, and "enumerated by the
   implementation PR" was two-implementations-diverge territory; the
   inline snapshot is the single reproducible authority, and updating it
   is a reviewed spec change); a
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
3. **Malformed mapped sections participate before value aggregation.** Each
   mapped section is syntax/version-validated independently. A
   malformed/truncated mapped section or mapped section at an unsupported
   version blocks every **grant** for every permission any field in that
   section maps to; in v1 that is P4. The blocker is global rather than
   jurisdiction-filtered because a decodable opt-out in the same section would
   be global under step 2. It never manufactures an opt-out, suppression, or
   destructive withdrawal. Valid opt-outs in other sections are still honored.
   Consequently valid national grant + malformed state, valid state grant +
   malformed national, and two valid grants + one malformed mapped foreign
   state all deny the affected grant. Unknown **unmapped** section IDs remain
   the explicitly bounded exception described above: they contribute nothing
   rather than making all future registry additions fail closed. This
   conservative mapped-section policy is product sign-off item 33.
4. **State-over-national, per field — for grants only:** where an
   applicable state section carries a field, its value governs that
   field's **grant** derivation; the national section fills only fields
   the state section lacks. This precedence **never suppresses an
   opt-out**: a national-section opt-out stands even where the state
   section's same field says not-opted-out — step 2's global rule wins,
   or a state string could erase a globally authoritative national
   opt-out.
5. **Aggregate across what remains applicable:** an opt-out (of either
   subclass) in any applicable field beats a grant from another —
   restrictive aggregation.

**OpenRTB `gpp_sid` construction is derived, never copied.** After the ordered
algorithm above, core constructs one sorted, duplicate-free integer array from
the pinned section IDs actually present in the decoded GPP header that either
(a) contributed a valid global opt-out or mapped-malformed grant blocker, or
(b) were applicable to the resolved transaction for grant evaluation. A pinned
and decoded GPP TCF section 2 is included when it supplied the effective TCF
record. Unknown unmapped IDs, foreign sections that contributed nothing, IDs
not present in the GPP header are omitted. A known mapped section at an
unsupported version remains identifiable from the decoded GPP header and is
included when its malformed-present blocker contributed; if the GPP header
itself cannot be decoded well enough to enumerate section IDs, no transport
pair is constructable. The serializer emits
`regs.ext.gpp` and `regs.ext.gpp_sid` atomically or emits neither; it never
sends raw GPP with a guessed, empty-by-error, or client-copied SID array.

The request companion `__gpp_sid`, when present, uses the exact ASCII grammar
`section-id *( "," section-id )`, where `section-id` is a positive base-10
integer without sign, whitespace, or leading zero. Input order is immaterial
and is canonicalized to a sorted set; a duplicate is malformed. The companion
is used only for consistency checking and is not the source of the OpenRTB
field. Exact set equality with the derived applicable set is accepted.
Absence is allowed because core can derive the set. A mismatch, duplicate,
non-decimal value, or reference to a mapped ID absent from the GPP header is a
malformed auxiliary signal: it blocks grants for the union of recognized
mapped permissions implicated by either set, preserves every decodable opt-out,
and never manufactures withdrawal. The derived set remains the only egress
value. Named fixtures cover absent companion, reordered/duplicate input,
foreign-state omission, global opt-out inclusion, section-2 inclusion,
mapped-version failure, and unknown-unmapped omission.

`SharingOptOut` and `TargetedAdvertisingOptOut` are new enforcement
inputs — current code consults only the sale field — and are declared as
such in the migration matrix.

#### 4.5.1 GPP registry snapshot (normative, vendored)

The pinned per-section accepted versions for the §4.5 map. This subsection is
the single reproducible authority; updating it is a
reviewed spec change. A mapped section presenting a version not listed
here is treated as malformed-present (§4.4).

| GPP section ID | Section                                             | Accepted version(s) |
| -------------- | --------------------------------------------------- | ------------------- |
| 6              | US Privacy string (uspv1, carried as a GPP section) | 1                   |
| 7              | usnat                                               | 1                   |
| 8              | usca                                                | 1                   |
| 9              | usva                                                | 1                   |
| 10             | usco                                                | 1                   |
| 11             | usut                                                | 1                   |
| 12             | usct                                                | 1                   |
| 13             | usfl                                                | 1                   |
| 14             | usmt                                                | 1                   |
| 15             | usor                                                | 1                   |
| 16             | ustx                                                | 1                   |
| 17             | usde                                                | 1                   |
| 18             | usia                                                | 1                   |
| 19             | usne                                                | 1                   |
| 20             | usnh                                                | 1                   |
| 21             | usnj                                                | 1                   |
| 22             | ustn                                                | 1                   |
| 23             | usmn                                                | 1                   |
| 24             | usmd                                                | 1                   |
| 25             | usin                                                | 1                   |
| 26             | usky                                                | 1                   |
| 27             | usri                                                | 1                   |

At the pinned commit below, the official section registry assigns IDs 24–27
to MD, IN, KY, and RI and each named state specification defines accepted
version 1. That commit-backed statement, rather than an unverified publication
month, is the authority for admitting them. Treating them as national-only
would discard a state-specific choice. Unknown IDs outside the accepted table
still contribute nothing and are flagged for snapshot review.

##### Provenance and vectors

The immutable authority is the official
`InteractiveAdvertisingBureau/Global-Privacy-Platform` commit:

`00ffaefe91513785e886c83877e9b56a4ec8e88c`

Normative upstream paths for the newly admitted layouts are:

- `Sections/US-States/MD/Maryland Privacy Technical Specification.md`
- `Sections/US-States/IN/Indiana Privacy Technical Specification.md`
- `Sections/US-States/KY/Kentucky Privacy Technical Specification.md`
- `Sections/US-States/RI/Rhode Island Privacy Technical Specification.md`
- `Sections/Section Information.md`

The implementation vendors decoder fixtures under
`crates/trusted-server-core/testdata/gpp/00ffaefe91513785e886c83877e9b56a4ec8e88c/`.
That directory contains a `manifest.json` object with:

- `upstream_commit_oid` and `upstream_commit_tree_oid`;
- a sorted `sources` array containing `{path, blob_oid, sha256_hex}` for all
  five normative paths above — the four state specifications and
  `Sections/Section Information.md`; and
- a sorted `cases` array whose entries are
  `{section_id, version, case, encoded, expected}`.

The vendoring PR description quotes the same commit/tree/blob values and the
independent command output used to verify every raw source SHA-256 and the
byte-for-byte copy. A commit OID without its tree and source-blob witnesses is
not accepted as completed provenance. `expected` uses the
permission spec's normalized P1/P4/GPC tokens, not decoder-library enums.
Fixture encodings must be constructed from the pinned bit layouts by an
independent generator or hand-checked vector, never emitted and consumed only
by the decoder under test. For every accepted section/version the corpus must
contain: minimum valid core-only string, core + GPC true, each mapped opt-out
value, each explicit not-opted-out value, explicit N/A, malformed/truncated
input, unsupported version, and a mixed known/unknown-section string. CI
rejects an update to this subsection unless the complete corpus for the new
commit is present.

## 5. Jurisdiction resolution

### 5.1 Order

Geo resolution runs **before** permission resolution — jurisdiction is an
input to the permission set, which is why geo providers cannot themselves be
gated on it (providers spec §5). A selected geo provider resolves country
and optional region; rules match `country/region` first, then `country`,
case-insensitively.

### 5.2 Lookup failure

Provider selected, lookup resolves nothing for a request → the compiled-in
protective failure profile applies: both permissions `requires_signal` and
`regime = "gdpr"`. `default_country` is not a provider-outage fallback; it
is used only for the explicitly acknowledged static-jurisdiction mode of
§5.3. An adapter whose geo
implementation can never resolve anything must not accept the selection at
all — that is the capability check of providers spec §6, and it prevents a
"selected but always empty" provider from silently converting every request
to §5.3 semantics without §5.3's guard.

The lookup-failure rate is exported as a metric and logged. A deployment may
use a bounded operational circuit breaker to stop auction dispatch during a
prolonged failure, but it may never substitute a permissive country rule for
unknown origin. Recovery to ordinary jurisdiction rules occurs only after a
successful live lookup.

Named divergence fixtures pin both sides of migration matrix row 5: lookup
failure with absent, malformed, expired, or regime-inapplicable evidence denies
both permissions and contextualizes dispatch; lookup failure with an explicit
valid regime-accepted grant may grant only its mapped permission under
`requires_signal`. The latter is a declared behavior change, never described
as preservation of today's deny-all path.

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

`[geo] default_country` is required only when no provider is configured and
`assume_single_jurisdiction = true`; startup fails without it in that mode.
It does not cover a selected provider's lookup failure (§5.2). Countries that
resolve but match no rule fall to the policy's `rules.default` entry
(§3.2). The two fallbacks are deliberately separate: "we could not place
this request" and "we placed it somewhere we have no rule for" are
different states, and pre-epic behavior treated them differently (fail
closed vs. non-regulated) — collapsing them is what made PR #838's
migration story unresolvable (migration spec §2, rows 5 and 7).

### 5.5 Policy revision activation

A **policy revision** has one identity used everywhere: the pair
**(content digest, activation ordinal)**. The digest is SHA-256 with
domain tag `tspol1|` over the canonical JSON of the parsed policy —
**canonicalization is RFC 8785 (JCS), referenced normatively**, not a
home-grown rule list (key ordering, number formatting, string
escaping, and Unicode handling are exactly JCS's), applied after
defaults are materialized, with a policy-schema profile making the
remaining cases unreachable: validation rejects non-finite numbers,
numbers outside the exactly representable integer range
(absolute value above 2^53 − 1) unless the field is string-typed, and
`null` values (materialized defaults mean `null` never appears);
negative zero serializes as JCS mandates. The machine-readable,
cross-language conformance fixtures are pinned inline in §5.5.1; every
runtime and the push tool must reproduce both the canonical UTF-8 bytes
and digest, and must reject every rejection vector before activation. A
digest difference is a startup failure, so canonicalization cannot be
approximately specified. The digest is pure content identity, so an
A→B→A rollback yields A's digest again.

The ordinal comes from the **policy/config/model activation register** —
deployment-metadata name `02` (providers spec §6.3), a linearizable
register holding `active`, an optional settings `candidate`, an optional
`model_candidate`, an `activation_journal_head`, and a bounded history of the
last 16 activations. `candidate` and `model_candidate` are mutually exclusive;
installing either while the other exists is rejected.
`active` is `{logical_root, immutable_blob_id,
source_version, data_hash, config_revision, policy_digest, ordinal,
model_epoch, minimum_binary_generation, row_schema_floor,
activation_generation}`. `activation_generation` is a logical active-tuple
`u64`, not the backing store's CAS/version token: installing a candidate or
readiness entry does not change it; each successful settings or model promotion
increments it by exactly one, and overflow is a hard deployment error. This is
the stable generation used by serve admission while candidate readiness is
changing. Each deployment additionally qualifies one
`serve_admission_lease_bound_ms`: the maximum interval from the
invocation of a successful linearizable admission read until every admission
derived from that read is locally invalid, including timer-rate error, delayed
response, suspend/resume, and every other adapter timing uncertainty. It is a
portable positive integer, is immutable while any member is traffic eligible,
and is not configurable through the settings blob; changing it requires
traffic to be stopped and the deployment capability to be requalified. Every
candidate snapshots the exact bound and every member readiness entry attests
it. Candidate installation rejects zero, a value different from the currently
qualified deployment bound, or an attempted bound change while any member is
traffic eligible. The activation register and immutable journal-object service
expose one authenticated, nondecreasing Unix-millisecond time domain; an
adapter with distinct or merely offset-local clocks is unqualified. The time
domain and its backing service are immutable while any member is traffic
eligible or a candidate exists; migration requires stopped traffic, no
candidate, and fresh qualification. The register's trusted clock qualification guarantees that its
promotion-not-before condition cannot become true until at least that much
real elapsed time after the draining CAS; forward steps, rate error, and
uncertainty fail the check rather than shorten the interval. `model_epoch` is
exactly `pre_epic_v1` or `permissions_v2`. Every binary exposes one immutable
monotonic `binary_generation` build constant. The initial N+1/N+2 generations
are 1/2, and values are never reused. A settings
`candidate` carries a `candidate_incarnation` of 32 lowercase hex characters
from 16 CSPRNG bytes, never reused in the deployment scope, plus the bound
active activation generation and complete active tuple, the complete proposed
content-binding tuple, copies the active model
fields byte-for-byte, and adds `proposed_ordinal`, the snapshotted
`serve_admission_lease_bound_ms`, an immutable authoritative
`fleet_snapshot`, phase (`preparing` or `draining`), readiness entries, and
quiescence entries. It also carries mutable `drain_attempt: u64`, initialized
to 0, and mutable `promotion_not_before_unix_ms`, null until each drain begins.
A `model_candidate` has its own never-reused candidate incarnation and
carries the exact bound active activation generation and complete active tuple,
all three proposed model fields, the immutable authoritative `fleet_snapshot`,
the same snapshotted admission-lease bound, phase, drain attempt,
promotion-not-before value, and readiness/quiescence entries. A snapshot
is `{membership_epoch, members[]}` where members are sorted unique stable
deployment-instance IDs. It is produced only by the authenticated deployment
membership controller from the set of instances eligible to receive traffic;
an application process, config publisher, or readiness writer cannot nominate
or remove members. Each readiness entry is authenticated as its member ID and
contains the membership epoch plus the complete immutable candidate identity
(every candidate field except mutable phase, drain attempt,
promotion-not-before, and the
readiness/quiescence maps). That identity includes the candidate incarnation;
a delayed entry from an aborted or restaged candidate can never validate even
when every content and fleet field is otherwise identical. The same member's
quiescence entry additionally binds the `draining` phase, exact nonzero
`drain_attempt`, and exact `promotion_not_before_unix_ms`, and is valid only
after that member has atomically closed new request admission and completed or
cancelled every request admitted under the bound activation generation.
Readiness cannot stand in for quiescence. The
immutable blob ID is the adapter mapping of `(logical_root, source_version)`;
`data_hash` is the verified envelope data hash, and `config_revision` is the
effective-config digest defined below. A readiness entry is therefore not bound
merely to source version and policy digest: a byte change, logical-root change,
config-only change, membership change, or ordinal change makes an old
acknowledgment inapplicable, while the `preparing` → `draining` phase CAS does
not. The register's 16-entry history is an operational
rollback window, not the audit or garbage-collection clock. Every promotion
also appends an immutable, hash-linked activation-journal record containing the
previous journal head, expected logical active `activation_generation`, complete
displaced and new tuples, membership epoch, readiness and quiescence sets,
the admission-lease bound and promotion-not-before time, retention horizon,
and controller identity. The journal object's qualified immutable-store
metadata supplies store-issued `created_at`; its canonical schema, object ID,
known-answer vector, lifecycle, listing, genesis, and pruning rules are
normative in §5.5.3. The controller writes and read-verifies that object
before promotion; the one register CAS both promotes the candidate and changes
`activation_journal_head` to its object ID. A losing CAS leaves an unreferenced
journal object, never an active tuple without a journal entry. Journal records
and every blob they name are retained for
at least 30 days and for at least the maximum processed-artifact, cookie-scope
migration, rollback, and audit horizon, whichever is longer. Rapid pushes can
evict a tuple from the 16-entry register but never shorten that time-based
retention. “Atomic” here means the register CAS binds the already verified
immutable journal object; it does not assume a cross-store transaction. The CAS
verifies the journal object's authenticated `created_at` in that common time
domain, rejects creation before the candidate's exact promotion-not-before
value, and rejects an object more than 60 seconds old. The earliest deletion
time adds that 60-second promotion allowance to the
required retention horizon, so even the latest permitted promotion receives
the full horizon. Local process time never starts or shortens retention. An
adapter without store-issued journal time, that binding, a qualified journal
listing/read path, and lifecycle enforcement is ineligible for multi-instance
activation. History and journal records never make an old configuration
eligible. `proposed_ordinal` equals the active ordinal for an unchanged policy
digest and active ordinal + 1 for a changed digest, with the overflow rule
below. Before the first activation, the compiled-in
protective configuration is the synthetic active tuple with logical root and
blob ID `builtin`, source version and ordinal 0, and reproducible
data/config/policy digests derived by the same grammars from materialized
compiled defaults; its model fields are `pre_epic_v1`, minimum binary
generation 1, row schema floor 1, and activation generation 0, and it permits
no destructive interpretation of historical evidence. Candidate abort is an
authenticated deployment-controller operation, is audit-recorded with the
complete candidate tuple and reason, and never rewrites `active` or reuses the
candidate's source version, blob identity, or candidate incarnation.

Every nonnegative integer carried into the activation journal — source version,
policy ordinal, model/binary/schema/activation generation, membership epoch,
drain attempt, admission-lease bound, promotion-not-before, readiness fields,
and retention — is additionally constrained
to the portable JCS range `0..=2^53-1`. The register may use a wider integer
primitive internally, but candidate installation rejects a value outside that
range; no activation can create an unjournalable active tuple.

`source_version` is an ordered `u64` within that portable range, scoped once per
deployment/application across all config-blob names. A single scope matches
the fixed deployment-metadata register key and avoids two independently
ordered streams aliasing one register.
Where the platform does not expose a trustworthy ordered push version, the
push tool allocates envelope `push_sequence` from a separate linearizable
**config-sequence register** in deployment metadata: strong-read + CAS
`next := current + 1`, then publish the envelope carrying `next`. Reaching the
portable maximum is a hard deployment error. Allocation
gaps after a failed publish are allowed; reuse is forbidden. A restore or
rollback republishes old content under a new sequence. The config store
itself is not assumed to provide conditional publication — its current
get/put/delete interface does not — and an adapter without the deployment-
metadata allocator is ineligible for multi-instance config/policy activation.
The CLI envelope design must add this field and allocator interaction.

Activation is **prepare then commit**, never “first instance wins”:

1. The push tool writes and read-verifies a new immutable envelope object,
   then CAS-installs its complete content-binding tuple as the sole candidate.
   Merely overwriting a mutable `app_config` key cannot stage or activate
   anything. A second candidate CAS is rejected until the current candidate
   activates or is explicitly aborted; an unreferenced object written by the
   loser is inert and later garbage-collected.
2. Every member of the candidate's immutable authoritative deployment-
   membership snapshot loads **that exact immutable object**, verifies the
   envelope data and sequence-binding hashes, materializes defaults, validates
   config and policy semantics, derives both config and policy digests, and
   CAS-records readiness for the complete candidate tuple. A member that
   cannot load the object or derives any different field fails closed and
   never acknowledges.
   Membership is frozen for that candidate. A new traffic-eligible member, a
   replacement instance with a new stable ID, or removal of a dead member
   changes `membership_epoch`, automatically aborts the candidate, and requires
   prepare to restart against a new snapshot. A controller cannot shrink a
   candidate in place to manufacture unanimity. Autoscaled instances may start
   during prepare, but receive no traffic until they load the current active
   object and enter the next authoritative membership epoch.
3. Only after every member in that snapshot is ready does the controller CAS
   the same candidate from `preparing` to `draining`, increments
   `drain_attempt` by one, atomically clears every quiescence entry, and, from
   the register's trusted store clock at that CAS's linearization point, sets
   `promotion_not_before_unix_ms` to the checked sum
   `store_now + serve_admission_lease_bound_ms`;
   overflow aborts and restages the candidate. At lease expiry, each member
   atomically stops admitting **all** new requests unless a renewal strong-read
   returns a non-draining result; a read that fails, is delayed past expiry, or
   observes `draining` cannot renew. The member's authenticated activation
   watcher also strong-reads the drain state, invalidates the local lease,
   closes admission, drains or cancels every request admitted under the bound
   activation generation, then writes its authenticated quiescence entry. A
   member may acknowledge only when no such
   request or its background work can still reach origin, bidder, partner,
   vendor, cache publication, identity mutation, or another configurable
   effect. A member that has observed `draining`, or whose prior admission
   lease has expired, gives new requests the deployment-unavailable response
   with no configurable egress. An unexpired lease may continue admitting the
   previous generation only until its hard bound; no admission read that
   linearizes after the drain CAS can create or renew such a lease. A member
   may acknowledge quiescence before the promotion-not-before time only when
   its own admission gate is already closed, but time alone never substitutes
   for that member's acknowledgment. A delayed watcher or acknowledgment
   extends the unavailable interval and cannot weaken the fence. A controller
   failure leaves admission closed until an authenticated
   `cancel-drain` CAS restores `preparing` and atomically clears every
   quiescence entry and the promotion-not-before value, a
   candidate abort removes the candidate, or a valid promotion completes; a
   local timeout cannot reopen traffic. Cancellation does not itself authorize
   traffic: each member must strong-read the restored phase before acquiring a
   new lease. Resumed traffic makes every earlier quiescence acknowledgment
   inapplicable.
4. Only after every member in that snapshot is both ready and quiescent, and
   the register's trusted store clock has reached the exact
   promotion-not-before value, does the deployment controller construct and
   read-verify the immutable journal object and
   CAS-promote the candidate to `active` while binding that object as the new
   journal head. The register itself rejects promotion while its trusted store
   clock is earlier than the candidate's exact
   `promotion_not_before_unix_ms`; a controller sleep or wall-clock comparison
   cannot satisfy this condition. Expiry of the bound only closes further old
   admissions — authenticated quiescence is still required to prove that the
   last admitted request and every asynchronous effect ended. If its policy digest
   differs from current `active`, promotion verifies
   `proposed_ordinal == max_ordinal + 1` and uses that new policy ordinal;
   mismatch or portable-range overflow is a hard deployment error, never wraparound. If
   the digest is unchanged, promotion verifies `proposed_ordinal` equals the
   current ordinal. For a
   config-only push, promotion advances `source_version` but retains the
   existing ordinal — unrelated configuration must not manufacture a new
   policy identity. A settings promotion copies the current model epoch,
   minimum binary generation, and row schema floor byte-for-byte; config cannot
   advance or roll them back, and sets activation generation to the bound
   generation + 1. **Every** promotion appends the complete previous active
   tuple to operational history, including config-only pushes. Audit and
   immutable-blob retention come from the independently time-bounded journal,
   so evicting operational history never loses the displaced snapshot.
5. Instances continue serving the entire previous active configuration while
   preparation is incomplete; the newest physical blob has no “latest wins”
   semantics, then stop during the explicit drain above. After promotion,
   members load and verify the new active tuple before reopening admission.
   **Every request**, including requests that do not use identity, must present
   a live admission validation when it atomically registers at the local gate.
   The validation covers both candidate phase and the complete `active` tuple,
   including `activation_generation` and model fields. An admitted request may
   outlive that validation's admission window; its gate/refcount registration,
   rather than a mid-request lease renewal, keeps it inside the drain and
   quiescence proof. With no live validation, admission strong-reads the
   register and may lease only a
   successful non-draining result for at most the deployment's qualified
   `serve_admission_lease_bound_ms`. Lease age starts no later than invocation
   of that linearizable read, never response receipt, so latency shortens the
   usable interval and a response arriving at or after expiry cannot admit.
   The lease binds the deployment, stable member ID, exact active tuple, and
   bound; it is process-local, is invalid after restart or suspend/resume, and
   cannot survive read, timer, or renewal uncertainty.
   `draining` rejects admission as above; otherwise the request uses settings
   loaded from that exact active blob whose data/config/policy hashes match.
   Local admission closing and request/background-effect registration use one
   atomic gate/refcount that compares the validation's exact active tuple with
   the gate's current tuple before incrementing, so quiescence cannot race a
   last admission and a delayed old-generation read cannot enter after reopen.
   The fence covers routing,
   auction serialization, integration selection, DataDome, response mutation,
   cache lookup/replay, identity, and destructive paths. The v1 fence's strong
   read may be amortized only by this activation-scoped lease; it grants no
   lease for authority, revocation, outbox, `w`, breaker, or other privacy
   state. A mismatch stops processing before origin,
   bidder, partner, or vendor egress, refreshes the complete settings object,
   and admits later requests only after every binding verifies. Failure to read
   or load active returns the deployment-unavailable response and performs no
   configurable egress. An
   instance starting or restarting likewise loads, verifies, and obtains a
   fresh admission validation on active before the traffic controller marks it
   serving. After promotion a member reopens only after loading the promoted
   tuple and strong-reading a lease for its new generation. No identity-only exception,
   mutable-root fallback, stale-on-error path, or partial per-subsystem refresh
   is permitted.

Model/writer activation is a second transition on the **same register**, so it
cannot race or drift from settings activation:

1. With new-shape settings active and no settings candidate, the controller
   CAS-installs a `preparing` `model_candidate`. Its immutable identity is the
   never-reused candidate incarnation, bound active activation generation,
   bound complete active tuple, proposed
   model epoch `permissions_v2`, proposed minimum binary generation 2,
   proposed row schema floor 2, snapshotted
   `serve_admission_lease_bound_ms`, and fleet snapshot; its mutable state is
   phase, drain attempt, promotion-not-before value, readiness, and
   quiescence. Re-entry at the same exact identity is idempotent; any different
   concurrent candidate is rejected.
2. Every traffic-eligible member in the authoritative snapshot must load the
   bound active settings, prove `binary_generation >= 2`, validate the v2
   provider/permission writer, and authenticate readiness for the complete
   model candidate. N+2 runs `pre_epic_v1` behavior until this promotion; it may
   not write v2 rows, positive authority, or durable use suppression merely
   because its binary understands them. A membership or active-settings
   activation-generation change aborts and restages the model candidate.
3. After unanimous readiness, the controller CASes the model candidate to
   `draining`, increments the model candidate's `drain_attempt`, and atomically
   clears old quiescence entries while setting the same store-clock
   `promotion_not_before_unix_ms`; every member applies the settings procedure's all-request
   admission stop, completes or cancels every `pre_epic_v1` request, and
   records bound quiescence. Local timeout cannot resume admission.
4. After unanimous readiness **and quiescence**, the controller creates the same immutable
   journal object required for a settings promotion and one register CAS
   verifies the promotion-not-before time has passed, the bound active
   activation generation, and complete tuple, changes
   `model_epoch`,
   `minimum_binary_generation`, and `row_schema_floor` together, clears the
   model candidate, increments `activation_generation`, appends operational
   history, and binds the journal head. Policy digest/ordinal and settings
   source version do not change.
5. Every serve-admission read checks the model fields as well as the settings
   tuple. After promotion, a binary with generation below the minimum stops
   before all request processing and cannot start serving; a qualifying N+2
   binary enables the new live gate/writer only after observing that exact
   active activation generation. There is therefore no interval in which N+1
   serves while N+2 writes v2. The old deployment-metadata `m00` schema-floor key is only a
   monotonic startup compatibility mirror written after this CAS; it has no
   authority to enable writes or serving. After a successful model CAS, the
   authenticated deployment controller owns the idempotent mirror completion
   step: strong-read `active` and `m00`; if `m00` is missing or lower, CAS it
   to exactly `active.row_schema_floor`, while equality is an idempotent
   no-op; then strong-read and verify exact equality before declaring the
   transition complete. A crash retries the same operation; it never lowers
   `m00` and never changes or authorizes the active register. An unreadable
   mirror or failed CAS/read-verification keeps startup closed and is retried.
   A mirror higher than active is rejected before any write and cannot be
   auto-lowered: startup fails for register/journal inconsistency
   investigation. The authoritative schema floor advances only in the single
   fenced model transition.

Head rules are therefore unambiguous: equal active `source_version` plus equal
data/config/policy digests adopts the active ordinal; equal version plus any
different digest or blob identity is a hard parse-divergence failure; lower
version is stale and rejected;
higher version is staged and is not active until fleet commit. A duplicate
publication never creates another ordinal. A higher-version config-only push
with the active policy digest retains the ordinal after readiness but changes
the active blob/data/config tuple. A→B→A remains a
third activation with a new `source_version`, A's digest, and a new ordinal
because A differs from the then-active B.

Authority wire records and S2S recomputation use the active policy `(digest,
ordinal)` pair. The hook's one complete cache revision tuple additionally binds
`model_epoch`, logical `activation_generation`, and its hook-invariant revision
exactly as the hook spec §3 defines, so model-only activation cannot replay a
pre-epic artifact. Its registry and config inputs are domain-separated SHA-256
hashes of JCS UTF-8 bytes: integration-registry revision = `tsreg1|` plus the
registration-order array of `{id, behavior_revision}`; config revision =
`tscfg1|` plus the complete typed effective config after defaults. Registry
array order is preserved because mutator order is behavior. The config form
contains secret **references**, never resolved secret bytes, and excludes
runtime observations. Both emit lowercase 64-hex digests and must reproduce
the inline revision fixtures in §5.5.2. Tests cover a
pre/post-model-CAS cache miss as well as concurrent push allocation, publish
gaps, equal-version idempotence, equal-version
digest mismatch, same-digest higher-version ordinal retention, stale restart,
candidate abort, partial readiness, promotion, old-instance behavior after
promotion, and A→B→A.

Mixed-revision irreversible behavior is **prohibited**, not accepted. Config
distribution may be mixed during preparation, but only the complete `active`
tuple authorizes identity decisions, and destructive effects are fenced to
that tuple (wire provenance records its policy pair). Rolling back acquisition
policy still cannot resurrect an identity
withdrawn by a valid user signal; rollback itself follows the same staged
activation protocol.

#### 5.5.1 Policy canonicalization vectors

The JSON object between the stable markers is the sole normative
machine-readable policy fixture. Extractors exclude the markers and code
fences, parse the enclosed UTF-8 JSON, and must reject duplicate object keys.

<!-- BEGIN NORMATIVE JSON: policy-canonicalization-v1 -->

```json
{
  "schema_version": 1,
  "canonicalization": "RFC 8785 (JCS)",
  "digest": "SHA-256",
  "domain_prefix_utf8": "tspol1|",
  "vectors": [
    {
      "name": "minimal-gdpr-policy",
      "effective_policy": {
        "rules": {
          "default": "gdpr"
        },
        "groups": {
          "gdpr": {
            "regime": "gdpr",
            "default": "requires_signal"
          }
        }
      },
      "canonical_json_utf8": "{\"groups\":{\"gdpr\":{\"default\":\"requires_signal\",\"regime\":\"gdpr\"}},\"rules\":{\"default\":\"gdpr\"}}",
      "sha256_hex": "68f72cc004bd59df7f799be24685b85cbbf5d5fbd1ba3069c8bf51adf4a88e6b"
    },
    {
      "name": "us-country-floor-and-state-override",
      "effective_policy": {
        "rules": {
          "default": "non-regulated",
          "US/CA": {
            "overrides": {
              "select-personalised-ads": "requires_signal"
            },
            "group": "us-opt-out"
          },
          "US": "us-opt-out"
        },
        "groups": {
          "us-opt-out": {
            "regime": "us-privacy",
            "default": "requires_signal"
          },
          "non-regulated": {
            "regime": "none",
            "default": "granted"
          }
        }
      },
      "canonical_json_utf8": "{\"groups\":{\"non-regulated\":{\"default\":\"granted\",\"regime\":\"none\"},\"us-opt-out\":{\"default\":\"requires_signal\",\"regime\":\"us-privacy\"}},\"rules\":{\"US\":\"us-opt-out\",\"US/CA\":{\"group\":\"us-opt-out\",\"overrides\":{\"select-personalised-ads\":\"requires_signal\"}},\"default\":\"non-regulated\"}}",
      "sha256_hex": "6c578c849c323936dc6d492449214c19e16e968b01a962c6ef99e9bbe3a08553"
    },
    {
      "name": "explicit-permission-map-without-default",
      "effective_policy": {
        "rules": {
          "default": "explicit"
        },
        "groups": {
          "explicit": {
            "regime": "none",
            "permissions": {
              "store-on-device": "granted",
              "select-personalised-ads": "requires_signal"
            }
          }
        }
      },
      "canonical_json_utf8": "{\"groups\":{\"explicit\":{\"permissions\":{\"select-personalised-ads\":\"requires_signal\",\"store-on-device\":\"granted\"},\"regime\":\"none\"}},\"rules\":{\"default\":\"explicit\"}}",
      "sha256_hex": "47745dbb0b5cf113e4d2eb9dda48e6fc9c6c8c18dc39ee715e9838edfd57727b"
    }
  ],
  "rejection_vectors": [
    {
      "name": "null-is-not-a-materialized-policy-value",
      "raw_json": "{\"rules\":{\"default\":null}}",
      "error": "null value"
    }
  ]
}
```

<!-- END NORMATIVE JSON: policy-canonicalization-v1 -->

#### 5.5.2 Configuration-revision canonicalization vectors

The same extraction rule applies to this sole normative registry/config
revision fixture.

<!-- BEGIN NORMATIVE JSON: revision-canonicalization-v1 -->

```json
{
  "schema_version": 1,
  "canonicalization": "RFC 8785 (JCS)",
  "digest": "SHA-256",
  "vectors": [
    {
      "name": "ordered-integration-registry",
      "domain_prefix_utf8": "tsreg1|",
      "normalized_value": [
        {
          "behavior_revision": 1,
          "id": "datadome"
        },
        {
          "behavior_revision": 2,
          "id": "prebid"
        }
      ],
      "canonical_json_utf8": "[{\"behavior_revision\":1,\"id\":\"datadome\"},{\"behavior_revision\":2,\"id\":\"prebid\"}]",
      "sha256_hex": "a2a81a6727226821d87f885c92410b5ebd2466e1060a070e027ad0eba210eff4"
    },
    {
      "name": "effective-config-hash-grammar-smoke",
      "domain_prefix_utf8": "tscfg1|",
      "normalized_value": {
        "integrations": {
          "datadome": {
            "secret_name": "datadome-api-key",
            "enabled": true
          }
        }
      },
      "canonical_json_utf8": "{\"integrations\":{\"datadome\":{\"enabled\":true,\"secret_name\":\"datadome-api-key\"}}}",
      "sha256_hex": "83c85084578ee35ddba12418c6337b7cc064b7022be5c8ffed068e94b07118d6"
    },
    {
      "name": "config-sequence-binding",
      "domain_prefix_utf8": "tscfgseq1|",
      "push_sequence": 42,
      "push_sequence_u64_be_hex": "000000000000002a",
      "data_hash_hex": "0000000000000000000000000000000000000000000000000000000000000000",
      "sha256_hex": "70edd94cd5728550a815355a1b719f4aafb466aa228571a4a7a6e88ad5178df0"
    }
  ]
}
```

<!-- END NORMATIVE JSON: revision-canonicalization-v1 -->

#### 5.5.3 Activation journal object and GC protocol

The journal uses the same qualified immutable config-object service under the
reserved logical root `ts_activation_journal`, never the mutable app-config
root or the identity graph. Its logical object ID is lowercase
`SHA-256("tsactj1|" || RFC8785-JCS-UTF8(journal))`; adapters map
`("ts_activation_journal", object_id)` to a write-once physical object. The
object materializes exactly these fields and rejects unknown/missing fields:

Every JSON number in the journal, including every number nested in an active
tuple, is an integer in `0..=9,007,199,254,740,991` (2^53 − 1). Booleans,
floats, negative values, and larger otherwise-valid `u64` values are rejected
before JCS; implementations may use wider internal integers but cannot emit
them here. Store-supplied lifecycle timestamps use the same portable range,
and addition that would exceed it fails closed. This profile makes the JCS
object ID identical in JavaScript, Rust, and every adapter rather than relying
on a language's larger integer type.

- `schema_version = 1`; `attempt_id` as 32 lowercase hex characters from 16
  CSPRNG bytes, allowing a timed-out attempt to publish a new object;
- `candidate_incarnation` as the exact candidate's never-reused 32 lowercase
  hex CSPRNG identity for `config`/`model`, or null for `checkpoint`;
- `previous_journal_id` and `pruned_through_journal_id`, each 64 lowercase hex
  or null under the link/pruning rules below;
- `expected_activation_generation: u64` and `transition_kind` exactly
  `config`, `model`, or `checkpoint`;
- `drain_attempt: u64`, which is the exact nonzero candidate drain attempt for
  `config`/`model` and zero for `checkpoint`;
- `serve_admission_lease_bound_ms: u64`, the exact positive
  deployment-qualified bound snapshotted by the candidate, and
  `promotion_not_before_unix_ms: u64`, the exact store-clock gate written by
  that drain attempt; both are zero only for a `checkpoint`;
- complete `displaced_active` and `activated_active` tuples from §5.5,
  including settings bindings, policy identity, model epoch, minimum
  binary generation, row schema floor, and logical activation generation;
- `membership_epoch: u64`, sorted unique `ready_members` and
  `quiesced_members` using the stable member grammar, authenticated
  `controller_id`, and `retain_for_ms: u64` constrained
  to at least 2,592,000,000 and the longest applicable artifact, cookie-scope,
  rollback, and audit horizon.

The cross-language known-answer and rejection vectors are inline in §5.5.3.1;
every controller, runtime verifier, and GC must reproduce both JCS bytes and
object ID and reject every numeric boundary vector. For the first promotion,
`previous_journal_id` is null only when the register head is
null and expected generation is zero. Every later config/model promotion must
name the exact current head and has null `pruned_through_journal_id`; the active
register CAS rejects any link/generation mismatch. For config/model entries,
`expected_activation_generation` must equal current active's logical
activation generation, and activated active must set it to that value + 1;
both member lists must equal the candidate snapshot's complete sorted member
list, `membership_epoch` must equal that snapshot's epoch, `drain_attempt` must
equal the candidate's current attempt and every quiescence acknowledgment, and
`candidate_incarnation` must equal every readiness/quiescence binding,
`serve_admission_lease_bound_ms` and `promotion_not_before_unix_ms` must equal
the candidate's exact drain fields, the admission-lease bound must be positive,
the immutable-store `created_at` for the journal must be at or after the
promotion-not-before time and no more than 60 seconds before the promotion CAS,
and the promotion CAS must independently enforce that its register store clock
has reached that time. These comparisons are defined only because the
activation register and immutable object service expose the same qualified,
authenticated Unix-millisecond time domain; adapters with incomparable clocks
fail activation qualification rather than comparing local timestamps.
Independently, `displaced_active` must equal current active and
`activated_active` must equal the candidate's computed post-CAS tuple. Overflow
is a hard error. A checkpoint
uses the current membership epoch, empty `ready_members` and
`quiesced_members` lists, and identical displaced and activated tuples
(including unchanged activation generation), with null
`candidate_incarnation`, `drain_attempt = 0`,
`serve_admission_lease_bound_ms = 0`, and
`promotion_not_before_unix_ms = 0`; it cannot stand in for fleet readiness or
quiescence.

The immutable store returns authenticated `created_at_unix_ms` object metadata
from the shared qualified activation time domain and maintains a separate,
extend-only `delete_not_before_unix_ms` lifecycle value. On every config or journal object
write, the adapter atomically initializes deletion protection to at least store
creation time + 30 days. For a promotion journal it extends protection for the
journal and both named blobs to at least `created_at + 60 seconds +
retain_for_ms` before the active CAS may bind the journal. These lifecycle
values can only increase. Therefore failed publication, aborted candidates,
losing journal attempts, and other unreferenced objects still have a store-clock
not-before value even though no successful promotion names them.

The object service's qualification supplies snapshot-consistent complete
listing for both logical roots: a listing returns one snapshot generation and
opaque pagination token; every page is from that generation, and mutation or
expiry of the token forces GC to restart without deleting. GC first completes
the listing, traverses and verifies the journal from the active head, and builds
the active/candidate/history/journal reachable set. Missing objects, broken
hashes/links, unknown schema, cycles, incomplete pages, or uncertain lifecycle
metadata abort the run. Deletion then uses object-ID CAS and is allowed only
when the object is unreachable and its store-enforced not-before has passed.

Journal pruning is an authenticated controller operation, never implicit GC.
Only when every record reachable from the current head is older than its full
retention horizon may the controller publish a `checkpoint` whose displaced
and activated tuples both equal current active, whose previous ID is null, and
whose `pruned_through_journal_id` is the old head. One register CAS verifies the
unchanged active tuple/generation and replaces only the journal head. The
checkpoint names and protects the current active blob; old journal objects
remain until their individual not-before values pass. Frequent activation can
therefore retain a longer chain but can never cut a still-required segment.

##### 5.5.3.1 Activation-journal vectors

The JSON object between the stable markers is the sole normative
machine-readable activation-journal fixture. Extractors exclude the markers
and code fences, parse the enclosed UTF-8 JSON, and must reject duplicate
object keys.

<!-- BEGIN NORMATIVE JSON: activation-journal-v1 -->

```json
{
  "schema_version": 1,
  "canonicalization": "RFC 8785 (JCS)",
  "digest": "SHA-256",
  "domain_prefix_utf8": "tsactj1|",
  "numeric_profile": {
    "type": "integer",
    "minimum": 0,
    "maximum": 9007199254740991
  },
  "vectors": [
    {
      "name": "genesis-config-promotion",
      "journal": {
        "schema_version": 1,
        "attempt_id": "00000000000000000000000000000000",
        "candidate_incarnation": "11111111111111111111111111111111",
        "previous_journal_id": null,
        "pruned_through_journal_id": null,
        "expected_activation_generation": 0,
        "drain_attempt": 1,
        "serve_admission_lease_bound_ms": 1000,
        "promotion_not_before_unix_ms": 1700000001000,
        "transition_kind": "config",
        "displaced_active": {
          "logical_root": "builtin",
          "immutable_blob_id": "builtin",
          "source_version": 0,
          "data_hash": "0000000000000000000000000000000000000000000000000000000000000000",
          "config_revision": "0000000000000000000000000000000000000000000000000000000000000000",
          "policy_digest": "0000000000000000000000000000000000000000000000000000000000000000",
          "ordinal": 0,
          "model_epoch": "pre_epic_v1",
          "minimum_binary_generation": 1,
          "row_schema_floor": 1,
          "activation_generation": 0
        },
        "activated_active": {
          "logical_root": "app_config",
          "immutable_blob_id": "app_config/1",
          "source_version": 1,
          "data_hash": "1111111111111111111111111111111111111111111111111111111111111111",
          "config_revision": "2222222222222222222222222222222222222222222222222222222222222222",
          "policy_digest": "3333333333333333333333333333333333333333333333333333333333333333",
          "ordinal": 1,
          "model_epoch": "pre_epic_v1",
          "minimum_binary_generation": 1,
          "row_schema_floor": 1,
          "activation_generation": 1
        },
        "membership_epoch": 7,
        "ready_members": ["edge-a", "edge-b"],
        "quiesced_members": ["edge-a", "edge-b"],
        "controller_id": "deploy-controller",
        "retain_for_ms": 2592000000
      },
      "canonical_json_utf8": "{\"activated_active\":{\"activation_generation\":1,\"config_revision\":\"2222222222222222222222222222222222222222222222222222222222222222\",\"data_hash\":\"1111111111111111111111111111111111111111111111111111111111111111\",\"immutable_blob_id\":\"app_config/1\",\"logical_root\":\"app_config\",\"minimum_binary_generation\":1,\"model_epoch\":\"pre_epic_v1\",\"ordinal\":1,\"policy_digest\":\"3333333333333333333333333333333333333333333333333333333333333333\",\"row_schema_floor\":1,\"source_version\":1},\"attempt_id\":\"00000000000000000000000000000000\",\"candidate_incarnation\":\"11111111111111111111111111111111\",\"controller_id\":\"deploy-controller\",\"displaced_active\":{\"activation_generation\":0,\"config_revision\":\"0000000000000000000000000000000000000000000000000000000000000000\",\"data_hash\":\"0000000000000000000000000000000000000000000000000000000000000000\",\"immutable_blob_id\":\"builtin\",\"logical_root\":\"builtin\",\"minimum_binary_generation\":1,\"model_epoch\":\"pre_epic_v1\",\"ordinal\":0,\"policy_digest\":\"0000000000000000000000000000000000000000000000000000000000000000\",\"row_schema_floor\":1,\"source_version\":0},\"drain_attempt\":1,\"expected_activation_generation\":0,\"membership_epoch\":7,\"previous_journal_id\":null,\"promotion_not_before_unix_ms\":1700000001000,\"pruned_through_journal_id\":null,\"quiesced_members\":[\"edge-a\",\"edge-b\"],\"ready_members\":[\"edge-a\",\"edge-b\"],\"retain_for_ms\":2592000000,\"schema_version\":1,\"serve_admission_lease_bound_ms\":1000,\"transition_kind\":\"config\"}",
      "sha256_hex": "7af3934b4e5500903ef77ad8a1367db83b03fc62a0bb83bd0849289c685d2e88"
    }
  ],
  "rejection_vectors": [
    {
      "name": "unsafe-top-level-u64",
      "base_vector": "genesis-config-promotion",
      "replace_json_pointer": "/expected_activation_generation",
      "raw_json_number": "9007199254740992",
      "error": "integer exceeds portable JCS profile"
    },
    {
      "name": "unsafe-embedded-active-u64",
      "base_vector": "genesis-config-promotion",
      "replace_json_pointer": "/activated_active/source_version",
      "raw_json_number": "9007199254740992",
      "error": "integer exceeds portable JCS profile"
    },
    {
      "name": "fractional-journal-number",
      "base_vector": "genesis-config-promotion",
      "replace_json_pointer": "/retain_for_ms",
      "raw_json_number": "2592000000.5",
      "error": "journal number is not an integer"
    },
    {
      "name": "unsafe-admission-lease-bound",
      "base_vector": "genesis-config-promotion",
      "replace_json_pointer": "/serve_admission_lease_bound_ms",
      "raw_json_number": "9007199254740992",
      "error": "integer exceeds portable JCS profile"
    },
    {
      "name": "zero-promotion-admission-lease-bound",
      "base_vector": "genesis-config-promotion",
      "replace_json_pointer": "/serve_admission_lease_bound_ms",
      "raw_json_number": "0",
      "error": "config/model admission lease bound is not positive"
    }
  ]
}
```

<!-- END NORMATIVE JSON: activation-journal-v1 -->

## 6. Failure-mode matrix — normative

| Condition                                                                                 | Resolution behavior                                                                           |
| ----------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| Geo lookup fails at request time (provider selected)                                      | Protective failure profile: both permissions `requires_signal`, `regime = "gdpr"`             |
| No geo provider configured                                                                | `default_country` baseline, guarded by §5.3                                                   |
| Country resolved, no matching rule                                                        | Policy `rules.default`                                                                        |
| Region resolved, no region rule                                                           | Country rule                                                                                  |
| No `[permissions]` section                                                                | Compiled-in fallback: everything `requires_signal`, `regime = "gdpr"`                         |
| S2S sync request (no user signals)                                                        | Authorized by stored provenance re-validated against current policy (§7)                      |
| Malformed policy                                                                          | Rejected at config push / startup (§3.3) — never per request                                  |
| No `default_country` in acknowledged no-provider static mode with a jurisdiction consumer | Startup failure; otherwise the field is not required (§5.3–§5.4)                              |
| Undecodable TCF/GPP record (present but malformed)                                        | Blocks grants (fail-closed acquisition, §4.4); never withdraws; opt-out signals still honored |
| Signals contradict (opt-out + consent)                                                    | Opt-out wins (§4)                                                                             |

The intended posture is fail-closed. Geo lookup failure uses the protective
profile, and the §5.3 static-jurisdiction configuration exists only behind
an explicit operator acknowledgment. Every other ambiguous state resolves
to the configured baseline or more restrictive.

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

   | Path                                                               | Required permissions                                                   | Notes                                                                                                                                                                                                                                                                                                                                                                                             |
   | ------------------------------------------------------------------ | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
   | OpenRTB `user.id`                                                  | `store-on-device` ∧ `select-personalised-ads`                          | Raw EC is identity in the bidstream — gated exactly as EIDs. PR #838 gated only EIDs, leaving `user.id` reachable with Purpose 4 refused                                                                                                                                                                                                                                                          |
   | EC-derived auction request IDs                                     | both purposes                                                          | Derived values are identity                                                                                                                                                                                                                                                                                                                                                                       |
   | Page-bids path                                                     | both purposes                                                          |                                                                                                                                                                                                                                                                                                                                                                                                   |
   | Bidstream EIDs                                                     | both purposes                                                          | The one gate PR #838 had                                                                                                                                                                                                                                                                                                                                                                          |
   | Proxy / click / Testlight forwarding of the EC cookie or headers   | both purposes                                                          | **New hardening, declared change** — these paths extract the raw cookie/header without today's jurisdiction gate (migration spec §2 row 11b)                                                                                                                                                                                                                                                      |
   | Identify endpoint (partner-facing)                                 | both purposes                                                          | Partner identity exchange, not a first-party lookup — decided here                                                                                                                                                                                                                                                                                                                                |
   | Pull sync (browser-request-scoped partner exchange)                | both purposes, from the **live** request resolution                    | Pull sync is created from a browser request and checks the live `EcContext` today — it keeps using the live P1 ∧ P4 decision plus the family revocation state (§4.3); stored provenance is never a substitute for available live evidence                                                                                                                                                         |
   | Batch sync (context-free S2S partner exchange)                     | both purposes, from **stored provenance**                              | The only truly signal-less path; authority rules below. Today's handler only authenticates and checks row state, so this gate is **declared hardening** (migration spec §2)                                                                                                                                                                                                                       |
   | Request-scoped graph reads/writes (non-revocation)                 | `store-on-device`                                                      |                                                                                                                                                                                                                                                                                                                                                                                                   |
   | Revocation paths (tombstones, withdrawal reads)                    | **exempt**                                                             | Must work when permissions are unset                                                                                                                                                                                                                                                                                                                                                              |
   | **Observability sinks — logs, traces, metrics, error attachments** | Never — no permission authorizes them                                  | Raw EC values (and derived URLs embedding them) must not reach any observability sink: logging boundaries accept redacted/hash-only types, not `&str` (PR #838 logs a redirect URL containing the EC and the raw `ec_id` field — the motivating counterexample); a log-schema denylist test enforces the row                                                                                      |
   | Stored consent-state lookup (§4.4)                                 | **exempt**, narrowly scoped                                            | Determining `store-on-device` cannot require `store-on-device`                                                                                                                                                                                                                                                                                                                                    |
   | Integration persistent response cookies                            | `store-on-device` (+ P4 where the cookie is an advertising identifier) | **Deferred with the hook's cookie surface** — the write-side gate alone was insufficient (read/use/forward/withdrawal unmodeled), so cookie operations ship only with the full model; this row and the client-cycle **page leg** (module injection gated on the provider's full declaration) join the inventory when their features do, and the §5.3 no-geo guard's consumer list grows with them |
   | Suppression-record writes (§4.3)                                   | **exempt**                                                             | Clearing authority is protective, like revocation                                                                                                                                                                                                                                                                                                                                                 |
   | Authority-state / suppression-decision read (§4.3)                 | **exempt**, narrowly scoped                                            | Returns only family ID, per-permission authority summary, and suppression entries — no identity values, no partner data; a test proves nothing else escapes                                                                                                                                                                                                                                       |
   | `AuthorityRefresh` provenance write (§4.3)                         | **exempt**, strictly scoped                                            | Commits current-live-resolution provenance only; enables suppression recovery without reopening `GraphOps`                                                                                                                                                                                                                                                                                        |

   **Raw regulatory transport is a separate positive allowlist.** The
   current allowlist contains only OpenRTB-compatible auction dispatch, and
   only the protocol field actually defined for that source: TCF in
   `user.ext.consent`, GPP in the atomic `regs.ext.gpp` + derived
   `regs.ext.gpp_sid` pair defined in §4.5, and
   US Privacy in `regs.ext.us_privacy`. A destination registration declares
   the fields it supports; TS sends the minimum matching source set, never a
   generic bundle of every raw signal. APS/direct auction APIs are not assumed
   equivalent to OpenRTB and receive no raw string until their checked-in
   protocol registration names the required field. Publisher origin,
   proxy/click/Testlight, identify and sync endpoints, ordinary integrations,
   identity rows, and observability sinks are explicitly not consumers.
   Unknown destinations default deny. Tests enumerate every allowed
   destination × field and assert every other egress view is structurally
   unable to access the raw strings.

   **Contextual OpenRTB is a positive projection, not “the ordinary request
   minus EC.”** Whenever auction dispatch is allowed while
   `select-personalised-ads` is unset, core serializes a
   `ContextualAuctionView` constructed independently from the ordinary auction
   object. The **sole normative v1 output schema** is the inline manifest in
   §7.1; descriptive
   prose cannot add a field. Its path language is dot-separated exact JSON
   member names, with `[]` denoting every element of the immediately preceding
   array. Object and array containers are implicit and exist only when at least
   one admitted descendant requires them; admitting a container never admits
   another child. `site` and `app` are mutually exclusive, and `imp` contains
   at least one element. V1 supports only the allowlisted banner and video
   impression shapes. A native, audio, or DOOH impression, or any impression
   that cannot be represented entirely by one allowlisted shape, makes the
   contextual serializer fail with no dispatch.

   Each manifest rule gives one exact leaf path, JSON scalar type,
   cardinality, derivation class, and, where present, the complete value enum.
   Cardinalities have these exact meanings: `required_single` is present once
   in its object; `optional_single` is absent or present once;
   `required_array` is a non-empty array whose every scalar element matches the
   rule; `optional_array` is absent or such a non-empty array;
   `required_array_member` is present once in every instance of the nearest
   enclosing object-array element; and `optional_array_member` is absent or
   present once in each such element. Empty optional arrays are omitted, never
   encoded. JSON numbers must be finite; integers use the OpenRTB field's
   declared range. Strings are valid UTF-8, are normalized by the field's
   OpenRTB grammar, and are rejected rather than truncated when they exceed its
   bound.

   The manifest's `cross_field_rules` are equally normative. Paths sharing an
   `[]` segment are evaluated within the same array-element binding, never
   across different impressions/nodes. `all_or_none` requires every named leaf
   or none; `required_nonempty_object_arrays` requires the named object array
   to exist with at least one element whenever its parent exists; and
   `at_least_one_complete_group` requires at least one listed group to be fully
   present whenever its parent exists. Thus GPP and its nonempty derived SID
   array are atomic, an emitted supply chain has at least one complete node,
   banner `w`/`h` are paired, and every emitted `banner.format` element has
   both dimensions. Unknown rule kinds or container paths fail startup.

   The derivation vocabulary is closed by that manifest. `fresh_transaction`
   is a new CSPRNG value for this dispatch and is never copied from or derived
   from EC, IP, consent, DataDome, graph, or stored provenance; `inventory` is
   a value from validated publisher inventory configuration, never a request
   free-form or extension value; `request_coarse` is only the typed coarse
   request fact named by the exact path; `privacy` is produced by the
   permission resolver or the raw-regulatory allowlist immediately above; and
   `constant` is the literal named by the rule. In v1 `device.lmt` is therefore
   exactly integer `1`. `device.os` has no version, `device.language` is one
   normalized primary language subtag, and `device.geo.country` is an uppercase
   ISO 3166-1 alpha-2 code; a finer or malformed source is omitted rather than
   rounded ad hoc.

   The serializer is generated from, or startup-validated byte-for-byte
   against, this manifest. A conformance walker expands every final encoded
   JSON leaf to the same normalized path and requires it to match exactly one
   rule with its type, cardinality, derivation tag, and enum, then evaluates
   every container and cross-field rule over the same final tree. An unknown,
   duplicate, ill-typed, untraceable, or unlisted leaf is a serialization error
   and produces **no bidder request**. This includes every unlisted `ext`
   member: there is no arbitrary JSON pass-through. V1 has no destination
   extension registrations; adding one requires a separate checked-in,
   machine-readable manifest using this same closed grammar and named for that
   destination. A destination that cannot consume this exact projection also
   receives no request — TS never falls back to the ordinary serializer.

   Consequently the v1 output has no EC or other user identifier, IP/IPv6,
   user agent, IFA, client forwarding header, hardware/network/screen
   fingerprint, precise geo, region/city/ZIP/metro, page/referrer/store URL,
   query/fragment, demographics, keywords, segments, custom data, or
   non-allowlisted extension. `user` can exist only as the implicit parent of
   the exact allowlisted `user.ext.consent` leaf when the destination's raw
   regulatory registration requires TCF. Conformance tests inspect the final
   encoded HTTP headers and body, poison every forbidden source (including
   nested extension objects), and prove that each poison is absent or the
   dispatch is suppressed.

   With **no EC provider configured**, identity use fails closed: a cookie
   value present on the request never egresses anywhere — never vacuously
   allowed (#838's `ec_allowed` was `is_none_or`, vacuously true with no
   provider).

   **S2S authority (batch sync).** A context-free server-to-server request
   carries no user signals, geo, or `EcContext`. Its authority is the
   **strong authority-state summary — the sole decision input** (the
   identity row's provenance fields are an audit mirror; reading
   decision inputs from an eventually consistent row was the two-source
   bug the revision fence exists to prevent): per-permission,
   time-bounded evidence written at mint and replaced on later live
   requests — grant basis
   (which signal class granted, per permission), the evidence's
   **authoritative timestamp and `valid_until`** (per evidence class),
   tagged jurisdiction provenance (defined below), and policy
   revision (the §5.5 pair) — **this list references the one normative
   summary schema, the providers spec §6.3 authority-state wire record;
   it is not a second schema** — and **not** provider/version,
   which lives only in the immutable mint tag, or a post-rotation visit
   would restamp a v1 identity as v2 (providers
   spec §6.1). Freshness is a **per-evidence-class contract**, because not
   every source carries a timestamp:

   | Evidence class                                    | Authoritative timestamp                                                                                                                                                                                                                                                                                                | Age reset                                                                                                                    | Max age                   |
   | ------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- | ------------------------- |
   | TCF consent                                       | The record's `LastUpdated`                                                                                                                                                                                                                                                                                             | Only a record with a **newer** `LastUpdated`                                                                                 | Existing TCF expiry TTL   |
   | GPP / USP values (no intrinsic timestamp)         | **First-seen**: when TS first observed this exact normalized value (a **per-permission equality digest computed over only the applicable, aggregated §4.5 fields for that permission** — never the whole GPP record, or a CMP touching an unrelated notice field would mint a new digest and reset first-seen forever) | Re-presenting an identical digest **keeps the original first-seen**; a different value is new evidence with a new first-seen | Consent TTL (same as TCF) |
   | Policy-baseline grant (`granted` rule, no signal) | The policy revision that granted                                                                                                                                                                                                                                                                                       | Re-derived on every recompute against the current revision — policy is not user evidence and does not age; it changes        | n/a                       |

   Timestamp handling is an **algorithm, not just a constant**: with
   skew S = 300 s (normative), a timestamp `t > now + S` renders its
   record malformed-present; `t` in `(now, now + S]` is used as-is (not
   clamped — clamping re-freshens replays); expiry checks grant a grace
   of S (`expired` means `valid_until < now − S`); and two evidence
   timestamps within S of each other **compare equal**, which routes
   the comparison to the tie rule (restrictive) — and the tie winner's
   **complete tuple survives unchanged** (state, source, timestamp,
   digest, `valid_until`): the losing grant's timestamp is never merged
   into the surviving refusal, or a sequence of near-window grants would
   ratchet the refusal's effective age forward and prolong it
   indefinitely. A slightly future-dated consent cannot out-order a
   just-observed opt-out;
   beyond-window future-dated records are **rejected as malformed**, and
   within the window a record's first normalized timestamp is pinned to
   its digest and never advanced by re-presentation (§4.3's anti-replay
   rule — clamping every presentation to "now" would make a future-dated
   string perpetually fresh). And every live
   resolution **atomically replaces the complete per-permission
   snapshot**, never merges — a refusal, opt-out, malformed or absent
   state in the fresh resolution clears prior positive authority for its
   scope, so an old P4 grant cannot survive a later P4 refusal. A sync request performs a **full recompute of both permissions from
   the strong summary alone** against the _current_ policy — the row
   contributes identity and partner data only after the
   `row.provenance_revision == summary_revision` fence passes:
   it fails closed when the stored jurisdiction's rule is now `denied`,
   when a `granted` baseline tightened to `requires_signal` and the stored
   evidence contains no accepted grant for that permission, when the
   stored evidence has **expired**, or when the regime no longer accepts
   the stored grant's source class (§4's regime-scoped table). Any of
   these → no update, row flagged for the operational cleanup of §4.2
   trigger 3. Sync never mints authority of its own. **Stored
   jurisdiction ages too**: batch sync has no live geo. The strong summary's
   jurisdiction is a tagged value, never an unlabelled country:
   - `Live { jurisdiction, provider_id, observed_at }` comes from a successful
     live geo lookup.
   - `StaticDefault { jurisdiction, config_revision, observed_at }` comes only
     from acknowledged §5.3 mode; S2S accepts it only while the active config
     revision still selects the same static jurisdiction.
   - `ProtectiveLookupFailure { provider_id, profile_revision, observed_at }`
     records §5.2 evaluation but **never authorizes context-free S2S egress**.
     A later successful live lookup must replace it first. The live request may
     still perform only what its protective-profile resolution authorizes.

   For `Live` or matching `StaticDefault`, batch sync recomputes against the
   stored jurisdiction from the last browser visit — and a visitor who moved
   from a permissive into a GDPR
   jurisdiction would otherwise keep old-rule egress for up to the row
   lifetime. A stored jurisdiction older than the **consent-TTL
   horizon** — age measured as now − the tag's `observed_at`, written by the
   browser-request resolution that produced `Live` or `StaticDefault`
   (providers spec §6.3; evidence timestamps are not a proxy: TCF
   `LastUpdated` can predate the live lookup, and a policy-baseline
   grant has no wall-clock evidence timestamp at all) — fails closed
   pending a live refresh (the inverse — denying
   a visitor who moved the other way — is the accepted cost); the
   horizon choice and its legal trade-off are **sign-off item 25**.

   **Legacy (pre-epic) rows** carry none of these fields. They are treated
   as reserved `hmac-v0` provenance with **no stored grant evidence**, so
   they **fail closed for partner egress and batch updates** until a live
   browser request lazily backfills provenance from a fresh resolution.
   That path is reachable by construction: a found legacy/v1 row whose
   authority record is a stub (or absent) is **denied egress but not
   indeterminate** — the permission-exempt `AuthorityRefresh` admits on
   the observed row plus the live resolution, never on a prior positive
   summary or matching revision, commits the summary, and the revision
   fence then opens use (providers spec §5 total state table). Failing
   open here would grandfather every pre-epic identity past the
   permission model indefinitely.

4. **Server-side auction dispatch** — gated on the policy `regime` class,
   normatively:

   | Regime                                                                                                                                                                                          | Dispatch rule                                                                                                                                                                                                                                                                                                                                                                       | Preserves                                                     |
   | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------- |
   | `gdpr`                                                                                                                                                                                          | Dispatch only with a decodable, unexpired TCF record consenting to Purpose 1. Malformed, expired, or absent record → **no bid request leaves** (no-bid response).                                                                                                                                                                                                                   | Today's GDPR/unknown arm                                      |
   | `us-privacy`                                                                                                                                                                                    | Dispatch proceeds in every signal state. When P4 is set, the ordinary egress inventory applies. When P4 is unset, including any mapped opt-out, dispatch is allowed only through the exact `ContextualAuctionView` above; serializer or destination-registration failure produces no bid request.                                                                                   | Today's dispatch posture, with contextuality made enforceable |
   | `none`                                                                                                                                                                                          | Dispatch proceeds.                                                                                                                                                                                                                                                                                                                                                                  | Today's non-regulated arm                                     |
   | **Any regime, TCF-sourced effective record** — a raw TC string on the request, a GPP section-2 hint (both detected **before decoding**), or a persisted-KV fallback record of TCF origin (§4.4) | The `gdpr` row applies: dispatch requires the _effective_ record to be decodable, unexpired, and consenting to Purpose 1. A **malformed or expired** raw signal therefore blocks dispatch — today a malformed raw TCF blocks, and gating this arm on decodability would have silently relaxed that. A US or non-regulated request carrying a Purpose 1 refusal is likewise blocked. | Today's raw-signal arm — **must not regress**                 |

   The **compiled-in fallback policy has `regime = "gdpr"`** (§3.1) — the
   no-policy posture must be the most protective for dispatch too, and a
   regime-less fallback would leave dispatch undefined. When dispatch is
   blocked, nothing leaves for that request: no PBS/APS call, no UA/IP/geo
   forwarding to bidders. When dispatch proceeds, what the request may carry
   is governed row-by-row by the egress inventory and, whenever P4 is unset,
   by the stricter positive contextual projection. Full regulatory
   strings are forwarded only to a destination whose protocol normatively
   requires them and whose registration declares it an authorized
   privacy-signal consumer; every other destination receives normalized
   outcomes or no regulatory field (§1).

The client-cycle resolve endpoint (own spec, currently on hold) would be a
further consumer if and when it proceeds.

### 7.1 Contextual OpenRTB v1 allowlist

The JSON object between the stable markers is the sole normative
machine-readable contextual projection. Extractors exclude the markers and
code fences, parse the enclosed UTF-8 JSON, and must reject duplicate object
keys. Descriptive prose elsewhere cannot add a field or relax a constraint.

<!-- BEGIN NORMATIVE JSON: contextual-openrtb-v1 -->

```json
{
  "schema_version": 1,
  "openrtb_version": "2.6",
  "path_grammar": "dot-separated exact JSON member names; [] denotes each array element",
  "default": "deny",
  "unknown_or_unlisted_behavior": "serialization_error_no_dispatch",
  "container_rules": {
    "implicit_parents_only": true,
    "omit_empty_optional_arrays": true,
    "minimum_imp_elements": 1,
    "site_app": "exactly_one",
    "supported_imp_media": ["banner", "video"],
    "each_imp_media": "exactly_one_of_banner_video",
    "unsupported_imp_media_behavior": "serialization_error_no_dispatch"
  },
  "cardinalities": {
    "required_single": "present exactly once in its object",
    "optional_single": "absent or present exactly once in its object",
    "required_array": "present non-empty scalar array; every element matches the rule",
    "optional_array": "absent or present non-empty scalar array; every element matches the rule",
    "required_array_member": "present exactly once in every nearest enclosing object-array element",
    "optional_array_member": "absent or present exactly once in every nearest enclosing object-array element"
  },
  "cross_field_rules": {
    "all_or_none": [
      ["regs.ext.gpp", "regs.ext.gpp_sid[]"],
      ["imp[].banner.w", "imp[].banner.h"]
    ],
    "required_nonempty_object_arrays": [
      {
        "when_parent_present": "source.ext.schain",
        "array_path": "source.ext.schain.nodes[]"
      }
    ],
    "at_least_one_complete_group": [
      {
        "when_parent_present": "imp[].banner",
        "groups": [
          ["imp[].banner.w", "imp[].banner.h"],
          ["imp[].banner.format[]"]
        ]
      }
    ]
  },
  "derivations": {
    "fresh_transaction": "fresh CSPRNG request value, never derived from request/user/security state",
    "inventory": "validated publisher inventory configuration only",
    "request_coarse": "typed coarse request value named by the rule",
    "privacy": "permission resolver or admitted raw regulatory transport only",
    "constant": "literal value named by the rule"
  },
  "rules": [
    {
      "path": "id",
      "type": "string",
      "cardinality": "required_single",
      "derivation": "fresh_transaction"
    },
    {
      "path": "at",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "tmax",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "test",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "allimps",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "cur[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "bcat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "badv[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "wseat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },

    {
      "path": "source.fd",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "source.tid",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "fresh_transaction"
    },
    {
      "path": "source.pchain",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "source.ext.schain.complete",
      "type": "integer",
      "cardinality": "required_single",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "source.ext.schain.ver",
      "type": "string",
      "cardinality": "required_single",
      "derivation": "inventory"
    },
    {
      "path": "source.ext.schain.nodes[].asi",
      "type": "string",
      "cardinality": "required_array_member",
      "derivation": "inventory"
    },
    {
      "path": "source.ext.schain.nodes[].sid",
      "type": "string",
      "cardinality": "required_array_member",
      "derivation": "inventory"
    },
    {
      "path": "source.ext.schain.nodes[].hp",
      "type": "integer",
      "cardinality": "required_array_member",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "source.ext.schain.nodes[].rid",
      "type": "string",
      "cardinality": "optional_array_member",
      "derivation": "fresh_transaction"
    },
    {
      "path": "source.ext.schain.nodes[].name",
      "type": "string",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "source.ext.schain.nodes[].domain",
      "type": "string",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },

    {
      "path": "imp[].id",
      "type": "string",
      "cardinality": "required_array_member",
      "derivation": "fresh_transaction"
    },
    {
      "path": "imp[].tagid",
      "type": "string",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].instl",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "imp[].bidfloor",
      "type": "number",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].bidfloorcur",
      "type": "string",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].secure",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "imp[].exp",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },

    {
      "path": "imp[].banner.w",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].banner.h",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].banner.format[].w",
      "type": "integer",
      "cardinality": "required_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].banner.format[].h",
      "type": "integer",
      "cardinality": "required_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].banner.pos",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].banner.topframe",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "imp[].banner.btype[]",
      "type": "integer",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "imp[].banner.battr[]",
      "type": "integer",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "imp[].banner.mimes[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "imp[].banner.api[]",
      "type": "integer",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },

    {
      "path": "imp[].video.mimes[]",
      "type": "string",
      "cardinality": "required_array",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.minduration",
      "type": "integer",
      "cardinality": "required_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.maxduration",
      "type": "integer",
      "cardinality": "required_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.protocols[]",
      "type": "integer",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.w",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.h",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.startdelay",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.placement",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.plcmt",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.linearity",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.skip",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "imp[].video.playbackmethod[]",
      "type": "integer",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.api[]",
      "type": "integer",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.battr[]",
      "type": "integer",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "imp[].video.pos",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },

    {
      "path": "imp[].pmp.private_auction",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "imp[].pmp.deals[].id",
      "type": "string",
      "cardinality": "required_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].pmp.deals[].bidfloor",
      "type": "number",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].pmp.deals[].bidfloorcur",
      "type": "string",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].pmp.deals[].at",
      "type": "integer",
      "cardinality": "optional_array_member",
      "derivation": "inventory"
    },
    {
      "path": "imp[].pmp.deals[].wseat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "imp[].pmp.deals[].wadomain[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },

    {
      "path": "site.id",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "site.name",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "site.domain",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "site.cat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "site.sectioncat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "site.pagecat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "site.mobile",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "site.privacypolicy",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "site.publisher.id",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "site.publisher.name",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "site.publisher.domain",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "site.content.cat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "site.content.language",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },

    {
      "path": "app.id",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "app.name",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "app.bundle",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "app.domain",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "app.ver",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "app.paid",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "app.cat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "app.sectioncat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "app.pagecat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "app.privacypolicy",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "inventory",
      "allowed": [0, 1]
    },
    {
      "path": "app.publisher.id",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "app.publisher.name",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "app.publisher.domain",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },
    {
      "path": "app.content.cat[]",
      "type": "string",
      "cardinality": "optional_array",
      "derivation": "inventory"
    },
    {
      "path": "app.content.language",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "inventory"
    },

    {
      "path": "device.devicetype",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "request_coarse"
    },
    {
      "path": "device.os",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "request_coarse"
    },
    {
      "path": "device.language",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "request_coarse"
    },
    {
      "path": "device.lmt",
      "type": "integer",
      "cardinality": "required_single",
      "derivation": "constant",
      "constant": 1
    },
    {
      "path": "device.geo.country",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "request_coarse"
    },

    {
      "path": "regs.coppa",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "privacy",
      "allowed": [0, 1]
    },
    {
      "path": "regs.ext.gdpr",
      "type": "integer",
      "cardinality": "optional_single",
      "derivation": "privacy",
      "allowed": [0, 1]
    },
    {
      "path": "regs.ext.us_privacy",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "privacy"
    },
    {
      "path": "regs.ext.gpp",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "privacy"
    },
    {
      "path": "regs.ext.gpp_sid[]",
      "type": "integer",
      "cardinality": "optional_array",
      "derivation": "privacy"
    },
    {
      "path": "user.ext.consent",
      "type": "string",
      "cardinality": "optional_single",
      "derivation": "privacy"
    }
  ]
}
```

<!-- END NORMATIVE JSON: contextual-openrtb-v1 -->

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
- The contextual serializer is tested as a positive schema against final
  encoded OpenRTB bytes. Fixtures place forbidden values in every ordinary and
  extension location (EC/derived IDs, user IDs/data/segments, IP/IPv6, UA,
  precise geo, URL/referrer/query, device IDs/fingerprints, and forwarding
  headers) and prove none survives; a destination without a qualified
  contextual registration produces no outbound request.
- GPP transport fixtures derive sorted unique `gpp_sid` from decoded
  applicability, never copy `__gpp_sid`, and assert atomic pair omission for an
  unconstructable set plus restrictive mismatch handling.
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
- Timestamp-less opt-out clearing: GPP/USP opt-out → later bare explicit
  not-opted-out presentation remains suppressed; a newer TCF `LastUpdated` or
  authenticated monotonic authorization revision clears → an identical
  timestamp-less opt-out presentation starts a new restrictive episode without
  refreshing its original first-seen, and the following no-signal live/S2S
  decisions remain suppressed; replay and equal revisions never grant.
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
