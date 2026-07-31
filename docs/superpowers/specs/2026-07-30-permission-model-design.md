# Design Spec: Jurisdiction Permission Model

**Status:** Draft
**Author:** Engineering
**Issue references:** #779
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-provider-migration-rollout-design.md`
**Last updated:** 2026-07-30

> **Context.** PR #838 proposed a permission model whose review surfaced two
> classes of defect this spec exists to prevent in the next pass: (1) silent
> behavioral inversions of consent-signal precedence — most seriously, a
> present TCF string short-circuiting GPC/GPP/US-Privacy opt-outs — and
> (2) fail-open jurisdiction resolution when geolocation is disabled. The
> precedence table (§4) and the failure-mode matrix (§6) are the two
> documents whose absence allowed those defects to hide in a 67-file diff.
> They are normative: an implementation whose behavior differs from these
> tables is wrong, whatever its tests say.

---

## 1. Overview

The permission model replaces the hard-wired jurisdiction gate
(`allows_ec_creation` and its companions) with a single resolved
**permission set** per request. Every data decision Trusted Server itself
makes — EC creation, EC withdrawal, EID transmission into the bidstream,
and provider execution (see providers spec §5) — reads that set.

The set is resolved from three inputs:

1. **Jurisdiction** — the country/region the request resolves to (§5).
2. **Policy** — a declarative, version-controlled map from jurisdiction to a
   baseline acquisition rule per permission (§3).
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
documented in the policy file header. Policy validation **rejects** a rule
that references an identifier outside the current vocabulary, so the file can
never promise more than the code enforces.

## 3. Policy

### 3.1 Format

A YAML map, embedded at build time, of named **groups** (baselines) and
**rules** keying countries (`FR`) and country/state pairs (`US/CA`) to a
group with optional per-permission overrides. Each permission resolves to an
**acquisition rule**:

- `granted` — set without any signal,
- `requires_signal` — set only when a signal grants it (opt-in),
- `denied` — never set, even when a signal grants it.

Overrides support all three targets: `+perm` (granted), `-perm` (denied), and
`~perm` (requires_signal). PR #838 supported only `+`/`-`, making the most
common real-world override — "this state requires a signal for personalized
ads" — inexpressible without duplicating a whole group. Groups may use a
`default:` shorthand for unlisted permissions.

### 3.2 Validation — at build time, not request time

The embedded file is validated by a `build.rs` step (or an equivalent
always-run CI test that asserts the parse explicitly): a malformed committed
file fails the **build**, never a request. PR #838's file was parsed lazily
behind a `OnceLock` with an `expect`, meaning a bad edit that escaped unit
tests became a 500 on every request.

Validation rejects:

- unknown fields anywhere (`deny_unknown_fields` on every deserialized
  struct — PR #838's untagged rule enum silently swallowed a misspelled
  `permission:` key, dropping the operator's override with no diagnostic);
- rule keys that are not plausible ISO 3166-1 alpha-2 / ISO 3166-2 codes;
- references to permissions outside the enforced vocabulary (§2);
- groups that neither list every permission nor provide `default:`.

### 3.3 One source of jurisdiction truth

The codebase currently carries a second, runtime-configurable jurisdiction
list (`consent.gdpr.applies_in`) used by the auction consent gate. Two
independently maintained country tables that both express "where GDPR
applies" will drift (in PR #838, adding `CH` to one had no effect on the
other). Requirement: either the auction gate derives its jurisdiction class
from the same resolved policy, or a CI test asserts that every country in
`applies_in` resolves to an opt-in (`requires_signal`) baseline in the policy
file, and vice versa for the shipped defaults.

### 3.4 Shipped table coverage

A CI test asserts every member of the GDPR country list resolves to an opt-in
baseline (this is what catches a `DK:` typoed as `DL:` — a defect that in
PR #838 survived parse, startup, and all tests, silently dropping Denmark to
the operator default). Countries intentionally not listed are governed by
§5's default-country rules; the policy header documents that this is the
fallback, and the shipped example default is the most protective baseline.

## 4. Signal precedence — normative table

Signals are classified:

- **Opt-out signals** (affirmative withdrawal): GPC header; GPP sections
  carrying a sale/sharing opt-out; US Privacy opt-out.
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
   revokes it.
4. Consent record grant — a TCF record present and consenting grants it
   (subject to 1–2).
5. No signal — the policy baseline decides: `granted` sets it,
   `requires_signal` leaves it unset.

### 4.1 Decision matrix

For each enforced permission, with baseline _B_ ∈ {granted,
requires_signal, denied}:

| Opt-out present | TCF present | TCF consents | Result                                             |
| --------------- | ----------- | ------------ | -------------------------------------------------- |
| yes             | —           | —            | **unset** (and withdrawal semantics apply, §4.2)   |
| no              | yes         | no           | unset (withdrawal applies only where §4.2 says so) |
| no              | yes         | yes          | set, unless B = denied                             |
| no              | no          | —            | set iff B = granted                                |

### 4.2 Withdrawal vs. absence

Two distinct outcomes, never conflated:

- **Withdrawal** (destructive: expire the EC cookie, write revocation
  tombstones) requires an **affirmative** signal: an opt-out signal, or a
  TCF record refusing `store-on-device` **in a jurisdiction whose baseline is
  opt-in** (`requires_signal`). A visitor who has simply not yet made a
  choice is never stripped of an existing identity.
- In a jurisdiction whose baseline is `granted`, a TCF refusal prevents
  _new_ grants but does not tombstone: tombstones are irreversible
  revocation markers, and PR #838 wrote them for visitors in unregulated
  jurisdictions whose global CMP emitted a purpose-refusing string —
  permanent identity loss under a regime the deployment never opted into.
- Withdrawal checking follows the same precedence as §4: an opt-out signal
  triggers withdrawal even when a consenting TCF record is present.

`ec_storage_withdrawn` (or its successor) gets direct unit coverage for every
row above; in PR #838 the headline "withdrawal expires identity" behavior had
no unit test at all.

## 5. Jurisdiction resolution

1. A selected geo provider resolves country and optional region; rules match
   `country/region` first, then `country`, case-insensitively.
2. **Provider selected, lookup fails for a request** → the configured
   `[geo] default_country` rules apply (per #779).
3. **No geo provider selected** → every request resolves to
   `default_country`. This turns jurisdiction into a static constant, which
   is only honest when the operator can genuinely assert single-jurisdiction
   traffic. Constraint: **startup fails** when no geo provider is selected
   _and_ the default country's baseline resolves any permission to `granted`,
   unless the operator sets an explicit acknowledgment
   (`[geo] assume_single_jurisdiction = true`). Without this, the natural
   migration config (`default_country = "US"`, geo unset) silently grants
   `store-on-device` and EID transmission to every EU visitor — the
   highest-severity finding of the PR #838 review. The startup log always
   prints the effective baseline and whether geo is live.
4. `default_country` is required; startup fails without it (per #779). The
   shipped example uses the most protective baseline.

## 6. Failure-mode matrix — normative

| Condition                                            | Resolution behavior                              |
| ---------------------------------------------------- | ------------------------------------------------ |
| Geo lookup fails at request time (provider selected) | `default_country` baseline                       |
| No geo provider configured                           | `default_country` baseline, gated by §5.3        |
| Country resolved, no matching rule                   | `default_country` baseline                       |
| Region resolved, no region rule                      | Country rule                                     |
| Malformed policy file                                | Build failure (§3.2) — unreachable at runtime    |
| No `default_country`                                 | Startup failure                                  |
| Undecodable TCF/GPP string                           | Treated as absent; opt-out signals still honored |
| Signals contradict (opt-out + consent)               | Opt-out wins (§4)                                |

The overall posture is **fail-closed**: every ambiguous state resolves to the
configured baseline or more restrictive, and the one configuration that could
convert "no information" into "granted" (§5.3) requires an explicit operator
assertion to exist.

## 7. Enforcement points

Exactly three consumers in this epic, all reading the same resolved set:

1. **Provider execution** (providers spec §5) — all three provider kinds.
2. **EC lifecycle** — creation requires `store-on-device`; withdrawal per
   §4.2.
3. **Bidstream EIDs** — transmission requires `store-on-device` ∧
   `select-personalised-ads`.

## 8. Testing strategy

- **The decision matrix is the test plan.** Every row of §4.1 and §4.2 ×
  each baseline, plus every row of §6, as table-driven tests. The ~24-case
  matrix deleted by PR #838 (net −18 tests in the consent module, replaced
  by happy-path cases only) is restored in equivalent form against the new
  API; signal-precedence conflicts (opt-out + consenting TCF) are mandatory
  cases, not optional ones.
- Policy validation tests for every §3.2 rejection.
- Shipped-table coverage test (§3.4) and split-brain consistency test
  (§3.3).
- One end-to-end integration scenario per posture: opt-in jurisdiction with
  and without consent, opt-out jurisdiction with GPC (including GPC + a
  consenting TCF string), and the no-geo/default-country path.

## 9. Out of scope

- Additional purposes (extension procedure in §2).
- Runtime-loadable policy (the embedded file is deliberate: policy changes
  are code reviews). If runtime policy is wanted later, it is its own spec
  with its own validation story.
