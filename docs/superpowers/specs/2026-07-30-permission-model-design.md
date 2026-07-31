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

| Identifier                | TCF purpose | Enforcement points                                                   |
| ------------------------- | ----------- | -------------------------------------------------------------------- |
| `store-on-device`         | 1           | EC provider execution; EC creation; withdrawal/tombstone eligibility |
| `select-personalised-ads` | 4           | EID transmission into the bidstream (jointly with `store-on-device`) |

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
`requires_signal` for every jurisdiction** — the most protective posture.
Absence of policy is always safe; there is no fail-open default.

### 3.2 Format

Named **groups** (baselines) and **rules** mapping a country (`FR`) or
country/state pair (`"US/CA"`) to a group, with optional per-permission
overrides. Each permission resolves to an **acquisition rule**:

- `granted` — set without any signal,
- `requires_signal` — set only when a signal grants it (opt-in),
- `denied` — never set, even when a signal grants it.

```toml
[permissions.groups.gdpr-eu]
default = "requires_signal"

[permissions.groups.us-opt-out]
default = "granted"

[permissions.rules]
FR = "gdpr-eu"
US = "us-opt-out"
# Overrides name explicit acquisition rules — no +/- sigil syntax; TOML
# expresses the target state directly.
"US/CA" = { group = "us-opt-out", overrides = { select-personalised-ads = "requires_signal" } }
# Reserved key: countries that resolve but match no rule. Distinct from
# [geo] default_country, which handles requests that resolve no country at
# all (§5.4).
default = "gdpr-eu"
```

A group's `default` covers unlisted permissions; a group may also name
permissions explicitly. Overrides map identifier → acquisition rule, so any
target state (including `requires_signal`) is expressible — PR #838's
`+`/`-` sigil scheme could not express "requires a signal", the most common
real-world override.

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
- references to undefined groups;
- groups that neither list every permission nor provide `default`.

### 3.4 One source of jurisdiction truth

Today, `detect_jurisdiction` — driven by the runtime lists
`consent.gdpr.applies_in` and `consent.us_privacy.states` — is the sole
jurisdiction source for **both** the auction consent gate and the EC gate.
The permission model replaces the EC side; if the auction gate keeps reading
the old lists while EC reads policy rules, the two will drift (adding a
country to one has no effect on the other, and an operator has no signal
that they disagree).

Requirement: the auction gate's jurisdiction class derives from the same
resolved policy (a country is GDPR-class when its rule resolves to an
opt-in baseline for `select-personalised-ads`). Where the legacy lists must
survive an interim period, a CI test asserts consistency between each list
and the policy table, with deliberate divergences recorded as explicit,
commented exceptions in the test — never silent. Both legacy lists are in
scope, not only the GDPR one.

### 3.5 Shipped-table coverage

A CI test asserts every member of the GDPR country list resolves to a
GDPR-class baseline in the example policy. This closes a defect class
nothing in PR #838's validation covered: a mistyped country key (`DL:` for
`DK:`) parses cleanly, starts cleanly, and silently drops a member state to
the fallback rule. Countries intentionally unlisted are governed by the
`rules.default` entry (§3.2); the example policy documents that fallback
inline, and ships it as the most protective baseline.

## 4. Signal precedence — normative

Signals are classified:

- **Opt-out signals** (affirmative withdrawal): GPC header; GPP sections
  carrying a sale/sharing opt-out; US Privacy opt-out. Opt-out signals are
  honored **globally**, not only in the jurisdictions whose law defines
  them — a deliberate, more-protective simplification: scoping a browser's
  explicit opt-out to a geolocation guess would honor it for some visitors
  and ignore it for others based on IP evidence. (For jurisdictions outside
  US states this is a declared behavior change; migration spec §2 records
  it.)
- **Consent records**: a decodable TCF string (standalone or embedded in
  GPP), which may grant or refuse individual purposes.

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
4. Consent record grant — a TCF record present and consenting grants it
   (subject to 1–2).
5. No signal — the policy baseline decides: `granted` sets it,
   `requires_signal` leaves it unset.

### 4.1 Decision matrix

For each enforced permission, with baseline _B_ ∈ {granted,
requires_signal, denied}:

| Opt-out present | TCF present | TCF consents | Result                                           |
| --------------- | ----------- | ------------ | ------------------------------------------------ |
| yes             | —           | —            | **unset** (and withdrawal semantics apply, §4.2) |
| no              | yes         | no           | unset (withdrawal per §4.2, trigger 2)           |
| no              | yes         | yes          | set, unless B = denied                           |
| no              | no          | —            | set iff B = granted                              |

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
   `requires_signal`.** Where the baseline is `granted`, refusal blocks
   _new_ grants but never tombstones: tombstones are irreversible, and
   PR #838 wrote them for visitors in unregulated jurisdictions whose
   global CMP emitted a purpose-refusing string — permanent identity loss
   under a regime the deployment never opted into.
3. **A policy edit is not a user signal.** Tightening a baseline to
   `denied` stops new identity but does not itself tombstone identities
   minted before the change; cleaning those up is an operational action
   (migration spec §6). An affirmative user signal (trigger 1 or 2) still
   withdraws them.
4. **Absence of signal never destroys identity.** A visitor who has not yet
   made a choice is never stripped of an existing identity.

Withdrawal checking follows §4 precedence: an opt-out signal triggers
withdrawal even when a consenting TCF record is present.
`ec_storage_withdrawn` (or its successor) gets direct unit coverage for
every trigger above; in PR #838 the headline "withdrawal expires identity"
behavior had no unit test at all.

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

### 5.3 No geo provider selected

Every request resolves to `default_country` — jurisdiction becomes a static
constant, which is only honest when the operator can genuinely assert
single-jurisdiction traffic. It is not only `granted` baselines that make
this dangerous: with a `requires_signal` baseline, a page-global CMP that
emits a consenting TCF string grants permissions for every mis-attributed
visitor just as effectively.

Constraint: **startup fails** when an EC provider is selected and no geo
provider is, unless the operator sets an explicit acknowledgment
(`[geo] assume_single_jurisdiction = true`). Stateless deployments (no EC
provider) are exempt. Without this guard, the natural migration config
(`default_country = "US"`, geo unset) silently grants `store-on-device` and
EID transmission to every EU visitor — the highest-severity finding of the
PR #838 review. The startup log always prints the effective baseline and
whether geo is live.

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

| Condition                                            | Resolution behavior                                          |
| ---------------------------------------------------- | ------------------------------------------------------------ |
| Geo lookup fails at request time (provider selected) | `default_country` baseline                                   |
| No geo provider configured                           | `default_country` baseline, guarded by §5.3                  |
| Country resolved, no matching rule                   | Policy `rules.default`                                       |
| Region resolved, no region rule                      | Country rule                                                 |
| No `[permissions]` section                           | Compiled-in fallback: everything `requires_signal`           |
| Malformed policy                                     | Rejected at config push / startup (§3.3) — never per request |
| No `default_country`                                 | Startup failure                                              |
| Undecodable TCF/GPP string                           | Treated as absent; opt-out signals still honored             |
| Signals contradict (opt-out + consent)               | Opt-out wins (§4)                                            |

The overall posture is **fail-closed**: every ambiguous state resolves to
the configured baseline or more restrictive, and the one configuration that
turns "no information" into a static jurisdiction assertion (§5.3) requires
an explicit operator acknowledgment to exist.

## 7. Enforcement points

Consumers of the resolved set in this epic:

1. **EC provider execution** (providers spec §5) — the provider's declared
   `required_permissions()` must all be set. This gate applies to EC
   providers only: geo and device providers execute **before** permission
   resolution as its inputs, so gating them on its output would be
   circular. Their governance is explicit selection, the capability checks
   of providers spec §6, and §2's vocabulary rule — if a future vocabulary
   adds a purpose covering geolocation or fingerprinting, gating those
   providers will require a two-phase resolution that must be specified
   then, not improvised.
2. **EC lifecycle** — creation requires `store-on-device`; withdrawal per
   §4.2.
3. **Bidstream EIDs** — transmission requires `store-on-device` ∧
   `select-personalised-ads`.

The client-cycle resolve endpoint (own spec, currently on hold) would be a
fourth consumer if and when it proceeds.

## 8. Testing strategy

- **The decision matrix is the test plan.** Every row of §4.1 × each
  baseline, every trigger of §4.2, and every row of §6, as table-driven
  tests. The ~24-case matrix deleted by PR #838 (net −18 tests in the
  consent module, replaced by happy-path cases only) is restored in
  equivalent form against the new API; signal-precedence conflicts
  (opt-out + consenting TCF) are mandatory cases, not optional ones.
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
