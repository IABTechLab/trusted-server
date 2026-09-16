# Design Spec: Jurisdiction Permission Model

**Status:** Proposed. PR #1045 carries the implementation and is not yet
merged to main. Revised against that implementation, 2026-08-25. Revised again
on 2026-09-01 for the `rules:` tree, which replaces the flat rule keys and
retires `[geo] default_country` (§3.2, §5.4, and the §12 record). Revised again
on 2026-09-14 for permission signal providers, which replace the fixed signal
precedence with a configured order (§4, and the §13 record).
**Author:** Engineering
**Issue references:** #779
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-provider-migration-rollout-design.md`
**Last updated:** 2026-09-14

> **Context.** PR #838 proposed a permission model whose review surfaced two
> classes of defect this spec exists to prevent. The first was silent
> inversion of consent-signal precedence, most seriously a present TCF string
> short-circuiting GPC/GPP/US-Privacy opt-outs through a rule fixed in code.
> The second was fail-open jurisdiction resolution when geolocation is
> disabled. Precedence is now explicit rather than silent, being the order a
> deployment configures, validated and logged at startup. The default order
> lets an answer given through a consent interface amend an opt-out the
> browser sent, and a deployment reverses that by listing the opt-out last
> (§4). The fail-open resolution is closed. The precedence rules (§4) and the
> failure-mode matrix (§6) are normative and are backed by tests in
> `crates/trusted-server-core/src/permission_signal/mod.rs`,
> `crates/trusted-server-core/src/ec/consent.rs` and
> `crates/trusted-server-adapter-axum/tests/permission_signals.rs`. One
> structural position of the 2026-07-31 draft was not adopted: policy remains
> a build-time-embedded
> `permissions.yaml`, not a `[permissions]` section of `trusted-server.toml`,
> because the runtime config push and activation apparatus the draft assumed
> does not exist yet (§3.1). Every other draft position that was narrowed,
> simplified, or deferred is recorded in §11, and the move to configured
> precedence in §13.

---

## 1. Overview

The permission model replaces the hard-wired jurisdiction gate
(`allows_ec_creation` and its companions, now removed) with a single resolved
**permission set** per request. The data decisions Trusted Server itself makes
through this model are EC provider execution, EC creation and withdrawal, EID
transmission into the bidstream, and sharing of the EC identifier beyond the
edge (§7). Server-side auction dispatch is not yet a consumer (§7.4).

The set is resolved from three inputs:

1. **Jurisdiction**, the country and optional region the request resolves to
   (§5).
2. **Policy**, a declarative map from jurisdiction to a baseline acquisition
   rule per permission, plus a declared signal policy (§3).
3. **Signals**, answered by the permission signal providers a deployment
   configures, in the order it configures (§4). Four implementations ship,
   one per scheme, being `gpc`, `gpp_sale_opt_out`, `us_privacy` and `tcf`,
   each a crate under `crates/permission-signal/`.

Issues #777 and #779 also envision publisher interaction and external services
as permission sources. The provider seam is that source interface. A provider
is a crate implementing `PermissionSignalProvider`
(`crates/trusted-server-core/src/permission_signal/mod.rs`) that answers
`Grant`, `Revoke` or `Neutral` for each permission, so a new source is a new
provider rather than a new resolution algorithm. Core names none of the four
shipped schemes. Each is registered by an integration builder, which is the
same seam a vendor crate uses, and a builder that supplies a permission
signal implementation does not have to be a page integration, so none of
these four crates ships browser JavaScript and none is named in
`[integration] provider`. §10 and §13 record the change.

Scope. The model governs decisions Trusted Server makes. A downstream protocol
receives the full regulatory context only where that protocol defines fields
for it (OpenRTB consent fields, and proxy-mode forwarding of raw strings).
The draft's stronger rule, that identity rows carry normalized per-permission
provenance and a digest and never a raw string, is **not yet true**: the
identity-graph entry (`KvEntry` in `ec/kv_types.rs`) stores the raw TCF and
GPP strings alongside the row today. The normalized provenance model travels
with the providers-spec storage work and is recorded as deferred (§11).

## 2. Vocabulary: the IAB Privacy Taxonomy Data Uses

Permissions are named by **IAB Privacy Taxonomy Data Uses**, mapped from the
IAB TCF Europe purposes and used strictly as technical identifiers. No CMP or
TCF policy is implemented by naming them. This replaces the draft's
TCF-purpose-identifier vocabulary (`store-on-device`,
`select-personalised-ads`). The taxonomy adoption postdates the 2026-07-31
draft and follows the joint taxonomy work with the IAB Tech Lab.

The implementation (`crates/trusted-server-core/src/permissions.rs`) models
the vocabulary in two tiers:

- **Eleven named permissions**, one per TCF purpose 1 through 11, each with a
  Data Use identifier. All eleven are resolved against the session signal
  (§4): a present TCF record grants or revokes each mapped purpose. Two
  purposes have no published Data Use yet, so purpose 1 uses a proposed
  `necessary.operations.storage` key and purpose 11 keeps its TCF identifier
  `select-basic-content`, both flagged for an upstream taxonomy addition.
- **Fifty-three additional taxonomy Data Uses**, carried so `permissions.yaml`
  can declare a policy flag for the whole taxonomy. No provider gates on them
  today and no signal maps to them, so their configured baseline stands.
  They exist for completeness, testing, and demonstration, and where no
  informed policy decision has been made the shipped file sets them `denied`.

The two Data Uses that carry enforcement weight today are
`necessary.operations.storage` (TCF Purpose 1, storage) and
`advertising_marketing.first_party.targeted` (TCF Purpose 4, personalized-ad
selection). Provider execution gates on whatever a provider declares (the
built-in Edge Cookie providers declare storage), and sharing beyond the edge
gates on the storage plus personalized-ad pair (§7).

This is a deliberate departure from the draft's rule that a permission appears
only when it has both a signal mapping and an enforcement point. The eleven
named Data Uses all have the signal mapping, and the fifty-three baseline-only
Data Uses are declared policy rather than enforced policy. The file header of
`permissions.yaml` states this plainly, and the `denied` default means an
undeployed flag cannot silently authorize anything. Policy validation still
**rejects** any rule or flag that references an identifier outside the modeled
vocabulary, so a policy cannot name a Data Use the code does not know.

The eleven named Data Uses, with the TCF purpose each maps from:

| #   | Data Use identifier                             | TCF purpose                                     |
| --- | ----------------------------------------------- | ----------------------------------------------- |
| 1   | `necessary.operations.storage`                  | Store and/or access information on a device     |
| 2   | `advertising_marketing.first_party.contextual`  | Use limited data to select advertising          |
| 3   | `advertising_marketing.profiling`               | Create profiles for personalised advertising    |
| 4   | `advertising_marketing.first_party.targeted`    | Use profiles to select personalised advertising |
| 5   | `advertising_marketing.personalize.profiling`   | Create profiles to personalise content          |
| 6   | `advertising_marketing.personalize.content`     | Use profiles to select personalised content     |
| 7   | `analytics.ad_reporting.measure_ad_performance` | Measure advertising performance                 |
| 8   | `analytics.ad_reporting.content_performance`    | Measure content performance                     |
| 9   | `analytics.ad_reporting.market_research`        | Understand audiences through statistics         |
| 10  | `necessary.operations.improve`                  | Develop and improve services                    |
| 11  | `select-basic-content`                          | Use limited data to select content              |

(The purpose names are the IAB names verbatim, including their original
spelling.)

## 3. Policy

### 3.1 Location: `permissions.yaml`, compiled into the build

Policy lives in a human-editable permissions YAML document (the repository
sample is `config/permissions/sample.yaml`, for testing and evaluation
only),
compiled into the binary with `include_str!` and parsed once per instance
(cached behind a `OnceLock` in `PermissionMaps::standard`). A deployer edits
or replaces the file and rebuilds to change policy. The file is not read at
runtime.

This keeps the mechanism the draft rejected, for a reason the draft's own
premise no longer supports. The draft required policy to flow through the
runtime config pipeline (`ts config push`, staged activation, §5.5), and that
activation apparatus does not exist. Publishing a `[permissions]` TOML section
with no activation protocol would reintroduce exactly the mixed-revision and
lazy-validation hazards the draft cataloged. Until the runtime pipeline
exists, the embedded file is the safer home, and moving policy to runtime
configuration is recorded as deferred follow-up (§11), not abandoned.

Within the embedded model, the draft's specific complaints are answered:

- The parse runs once per instance, and the embedded file is a build-time
  constant covered by unit tests, so a malformed committed file fails the
  test suite rather than surfacing as a per-request failure. The documented
  panic on a malformed embedded file is a build defect signal, not a runtime
  condition.
- Unknown fields on a detailed rule are rejected (`deny_unknown_fields`), so
  a misspelled override key fails loudly instead of being swallowed.
- Two rule keys naming the same location in different case are rejected at
  parse, so one spelling cannot silently overwrite another.
- Auditability lives where the draft placed it, in version control. The file
  ships in the repository, and its history is the change log.

The `include_str!` path still reaches above the crate root, so the crate
packaging concern the draft raised remains open and moves with the runtime
follow-up.

**Fallback posture.** The file is always present, so there is no "no policy"
state. Every location resolves to a group, because the tree's top node is
required and states the baseline for a request whose country cannot be
resolved (§3.2). A failed geo lookup is the separate state, and it resolves
every permission to the **requires-signal floor**, where nothing is set without
a signal that grants it. That floor also covers the belt-and-braces case where
resolution somehow finds no node at all. Absence of an applicable node is
always safe, and there is no fail-open default. The
draft's `regime = "gdpr"` component of the protective profile has no
implemented counterpart because no regime concept exists (§3.2).

### 3.2 Format

Named **groups** (baselines) and a **rules** tree that maps places to a group,
plus a **signals** section that declares how each session signal maps onto Data
Uses. Each permission resolves to an **acquisition rule**:

- `granted`, set without any signal,
- `requires_signal`, set only when a signal grants it (opt-in),
- `denied`, never set, even when a signal grants it.

```yaml
# permissions.yaml (abbreviated). The shipped file lists every Data Use in
# every group so each group's meaning is fully explicit.
groups:
  gdpr-eu:
    necessary.operations.storage: requires_signal
    advertising_marketing.first_party.targeted: requires_signal
    # ... every remaining Data Use, requires_signal or denied
  gdpr-uk:
    necessary.operations.storage: granted
    # ... the other mapped purposes requires_signal, the rest denied
  us-opt-out:
    necessary.operations.storage: granted
    advertising_marketing.first_party.targeted: granted
    # ... the other mapped purposes granted, the rest denied

rules:
  group: gdpr-eu # baseline when no country can be resolved
  jurisdiction: gdpr # inherited by every node that states none
  FR: gdpr-eu
  GB: gdpr-uk
  AU: us-opt-out
  US:
    group: us-opt-out
    jurisdiction: non-regulated
    # A region node takes precedence over its country, and a state with a
    # comprehensive privacy law names itself with `jurisdiction: us-state`. A
    # node written as a mapping may also apply explicit per-permission
    # acquisitions on top of its group:
    # CA:
    #   group: us-opt-out
    #   jurisdiction: us-state
    #   permissions:
    #     advertising_marketing.first_party.targeted: requires_signal

signals:
  tcf:
    authoritative: true
  us_opt_out:
    sources: [gpc, gpp_sale_opt_out, us_privacy_opt_out]
    revokes:
      - necessary.operations.storage
      - disclosure.sale
      # ... sharing, third-party sale, targeted advertising and profiling
```

Format rules, as implemented:

- A group is a flat map of Data Use to acquisition flag, with an optional
  `default` key covering any permission the group omits. A group without
  `default` must list **every** modeled permission exactly once, or the
  parse fails (`IncompleteGroup`). The shipped groups list every Data Use.
- `rules` is one tree. Every node has a `group`, and children are optional.
  Child keys are place codes, so the first level holds ISO 3166-1 alpha-2
  country codes and the levels beneath a country hold ISO 3166-2 region codes
  with no country prefix. Codes are matched case-insensitively.
- A node whose value is a single string has that string as its `group` and no
  children. A node written as a mapping must contain `group:` and may contain
  child place codes beside it. Reserved keys cannot collide with place codes,
  because an ISO code is at most three characters.
- Any node may carry `jurisdiction:` beside `group:`, naming the consent
  handling for the places that node covers. A node that carries none inherits
  the nearest ancestor that does. The top node, meaning the `rules:` mapping
  itself, must carry both, so the inheritance always terminates and the
  unresolved-country case is simply the top node.
- The accepted `jurisdiction` values are the states of the `Jurisdiction` type
  in `crates/trusted-server-core/src/consent/jurisdiction.rs`, being `gdpr`,
  `us-state`, `non-regulated`, and `unknown`. Any other value is a
  configuration error. `us-state` carries **no** state code, because the region
  node it sits on already names the state, so the value is valid only on a
  region node under `US` and is rejected at the top and on a country node.
- Matching is most specific wins with fallback to the parent, being region,
  then country, then the top node. `group` and `jurisdiction` fall back
  independently, so the two answers need not come from the same node.
- A mapping node may carry `permissions`, a map from a Data Use to an explicit
  acquisition (`granted`, `requires_signal`, or `denied`), overriding the group
  baseline for exactly that Data Use. This adopts the draft's requirement that
  overrides name explicit target states. The earlier `+`/`-` sigil scheme,
  which could not express `requires_signal`, is gone.
- The **signals** section is new relative to the draft. The opt-out source
  list and the opt-out revoke set are data in the file, and the
  `signals.tcf.authoritative` flag governs only whether a present TCF
  record's own grants and revokes apply. The TCF purpose to Data Use mapping
  is not in the file. It lives in the TCF provider crate
  (`crates/permission-signal/tcf/src/mapping.rs`), so a deployment that runs
  no TCF carries no table of another scheme's numbers. Nor is the order the
  signals are asked in, which is `[permission_signal] provider` in
  `trusted-server.toml` (§4). The `us_opt_out.revokes` value is `all` or an
  explicit list of Data Uses, so a deployer bounds what an opt-out drops.

The canonical shape of the tree, as written for policy owners in
`docs/guide/permission-model.md`:

```yaml
rules:
  group: gdpr-eu
  # Inherited by every country not overriding it, and the answer when no
  # country resolves.
  jurisdiction: gdpr
  GB: gdpr-uk # inherits gdpr
  US:
    group: us-notice
    jurisdiction: non-regulated
    CA:
      group: us-opt-out
      jurisdiction: us-state
    NY: us-notice # inherits non-regulated
```

A Manchester visitor gets `gdpr-uk` under GDPR handling, a Californian visitor
gets `us-opt-out` under that state's own opt-out handling, a Texas visitor gets
`us-notice` under `non-regulated` handling through the US fallback, and a
visitor with no resolvable country gets `gdpr-eu` under GDPR handling. The
nesting shows a reader the precedence rather than hiding it in slash-joined
keys, and the audience for the file is a policy owner rather than a developer.

The draft's required per-group **`regime`** class (`gdpr`, `us-privacy`,
`none`) is **not implemented**. Its intended consumer, server-side auction
dispatch, was not migrated to the permission model (§7.4), so the field would
be inert today. It returns with the dispatch migration.

### 3.3 Validation

Validation runs where the policy actually enters the system:

- **At parse**, meaning the unit tests and any `PermissionMaps::from_yaml`
  caller, the file is rejected for: malformed YAML, a rule referencing an
  undefined group, an unknown Data Use identifier anywhere (group flag,
  detailed-rule entry, or revoke list), an acquisition
  value outside `granted | requires_signal | denied`, a group without
  `default` that does not list every permission, duplicate rule keys under
  case-insensitive comparison (`us` and `US`), unknown fields on a detailed
  rule, and a `revokes` keyword other than `all`.
- **At startup**, the top node of the compiled `permissions.yaml` must carry
  both a `group` naming a defined group and a `jurisdiction`, and the no-geo
  acknowledgment must be present where required (§5.3). A file whose top node
  is missing either key is rejected exactly as a missing
  `[geo] default_country` was rejected before. Both are settings-construction
  failures, never per-request failures. A name in `[permission_signal] provider`
  that the build does not carry, or a name given twice, fails when the
  adapter builds its state at startup, also never per request (§4).

Not implemented from the draft's list, and recorded as future hardening
(§11): checking rule-key country parts against the assigned ISO 3166-1 list,
checking region parts against assigned ISO 3166-2 subdivisions, and the group
identifier grammar. A mistyped country key (`DL` for `DK`) therefore still
parses. For the shipped table the EU and EEA coverage test (§3.5) closes the
consequence the draft cared about, a member state silently dropping to the
fallback.

### 3.4 One source of jurisdiction truth (adopted 2026-09-01)

The runtime lists `consent.gdpr.applies_in` and
`consent.us_states.privacy_states` retire into the `rules:` tree, so one
document now carries the permission baselines and the regime applicability
together. The shipped policy file states the same 31 GDPR countries, being the
EU 27 plus Iceland, Liechtenstein, Norway and the United Kingdom, which inherit
`gdpr` from the top node, and the same 20 US states with a comprehensive
privacy law, written as region children of `US` carrying
`jurisdiction: us-state`. A visitor resolves to one node, and that node answers
both what is permitted and which regime applies, so the drift the draft named,
where a country added to one source had no effect on the other, has nothing
left to drift against and needs no consistency test between two lists.

What remains open is the consumer side. Auction dispatch still reads the
consent subsystem's gate rather than a policy regime class (§7.4), so unifying
the last consumer travels with the dispatch migration. Documentation that
pointed operators at the two `[consent]` lists points at the tree instead.

### 3.5 Shipped-table coverage

Implemented as a unit test
(`every_eu_and_eea_member_requires_a_signal_for_storage` in
`permissions.rs`): every one of the 27 EU member states plus the three EEA
members (IS, LI, NO), 30 codes in all, must have a rule, and each must
resolve `necessary.operations.storage` as `requires_signal`. A mistyped
member-state key fails this test rather than silently diverting the country
to the deployer default. The shipped table maps the EU 27 and EEA to
`gdpr-eu`, the UK to `gdpr-uk` (storage `granted`, a baseline the rollout
ledger row 4 asks the task force to confirm with its citation, everything
else opt-in), and the US and Australia to
`us-opt-out`. Countries with no node of their own fall to the tree's top
`group` (§5.4).

## 4. Signal precedence (normative, implemented)

Precedence is the **order a deployment configures**, not a rule in code. The
policy file decides what the shipped schemes mean for the deployment (§3.2),
each permission signal provider decides what its own scheme says, and
`[permission_signal] provider` in `trusted-server.toml` decides the order the
providers are asked in. Each permission resolves in these steps.

1. **Policy `denied`** is never set, regardless of any signal. (Enforced in
   the resolver, `PermissionMaps::resolve_with`.)
2. **A consent record present but undecodable revokes everything**, ahead of
   every configured provider and whichever of them are configured
   (`permission_signal` in `crates/trusted-server-core/src/ec/consent.rs`). A
   malformed record is a preference that could not be read, so it fails
   closed rather than degrading to the no-signal baseline, which under a
   `granted` baseline would turn garbage into a grant. It is error handling
   rather than a signaling scheme, so it takes no place in the order and
   cannot be removed, which also stops a readable record from one scheme
   overwriting the refusal an unreadable record from another caused. It never
   withdraws (§4.2). An **expired** TCF record is deliberately a distinct
   state, not malformed. The decoded record is cleared, the raw string is kept
   for proxy forwarding, and acquisition proceeds as if the record were
   absent, so the baseline applies.
3. **The configured providers are asked in order** (`combine` in
   `crates/trusted-server-core/src/permission_signal/mod.rs`). Each sees the
   baseline and what the providers before it settled on, and answers `Grant`,
   `Revoke` or `Neutral` for the permission. A `Neutral` answer leaves the
   prior value standing and any other answer replaces it, so **the last
   provider with an opinion decides**. Every provider is asked, because a
   later one may amend what an earlier one settled.
4. **No provider with an opinion leaves the baseline standing**, so `granted`
   sets the permission and `requires_signal` leaves it unset. A `Revoke` drops
   a `granted` baseline, and only a `Grant` sets a `requires_signal` one.

The order is set by `[permission_signal] provider`, which takes a list rather
than a single name because several permission signal providers run, and the
list is the order they run in:

```toml
[permission_signal]
provider = ["gpc", "gpp_sale_opt_out", "us_privacy", "tcf"]
```

- **Omitted**, every provider the build carries runs, in the order the
  adapter offers them, so a signal is never ignored because nobody listed it.
- **An empty list** runs no provider, and every permission stays at its
  country and region baseline.
- **A name the build does not carry, or a name given twice**, fails startup
  with a message listing the providers that are available.
- The providers acted on, and any the list leaves out, are written to the log
  once at startup.

None of the four takes a `[permission_signal.<name>]` table, because none of
them has a setting to carry. A scheme that did would take one, under the rule
in the providers spec §2.1.

Every adapter offers the four shipped providers in the same default order,
being `gpc`, `gpp_sale_opt_out`, `us_privacy` and `tcf`. Global Privacy
Control is a browser setting with no interface of its own, so it is asked
first, and the three schemes that carry an answer a person gave through an
interface are asked after it. Under that default a TCF record consenting to a
mapped purpose sets a permission that a GPC header, a GPP sale opt-out or a US
Privacy opt-out revoked, and a deployment that wants the opt-out to stand
lists it after `tcf`. Which scheme wins is a question about a jurisdiction and
a publisher, so the deployment decides it. The `signals.tcf.authoritative`
flag governs only TCF's own effect, and with it false the TCF provider has no
opinion.

A provider is also given the whole ordered list and its own place in it, and
may ask a named peer what that peer makes of a permission. That makes a rule
such as "this answer supersedes a refusal only when Global Privacy Control
made it" expressible inside the provider that wants it. A consultation goes
one level deep, so two providers asking each other cannot loop.

Removing `gpc` from the list stops the GPC provider running, but the consent
pipeline can still synthesize a US Privacy opt-out from the same header for a
US privacy state (§4.4), which the `us_privacy` provider then acts on. A
deployment that wants the header to have no effect also turns that synthesis
off.

Two simplifications against the draft's taxonomy, both recorded in §11:

- **Of the shipped providers, TCF is the only grant-class signal.** The
  draft's grant class also admitted explicit GPP/USP non-opt-out values,
  regime-scoped, so a US rule could be `requires_signal` yet grant on
  signal-carrying traffic. The implementation instead expresses the US posture
  as a `granted` baseline that opt-outs revoke, so no-signal US traffic is
  allowed rather than blocked pending a signal. Explicit non-opt-out GPP/USP
  values grant nothing on their own.
- **Malformed-present blocks everything, not per family.** Any present but
  undecodable record (TCF, GPP, or US Privacy) revokes every Data Use for
  the request, rather than blocking only the permissions mapped to the
  malformed source. This is strictly more restrictive than the draft's
  per-family rule.

### 4.1 Decision matrix

For each permission, with baseline _B_ from the resolved rule, under the
default order:

| Signal state                                                            | B = granted | B = requires_signal | B = denied |
| ----------------------------------------------------------------------- | ----------- | ------------------- | ---------- |
| Any record present but undecodable                                      | unset       | unset               | unset      |
| TCF present, consents to the mapped purpose, with or without an opt-out | set         | set                 | unset      |
| TCF present, refuses the mapped purpose                                 | unset       | unset               | unset      |
| Opt-out present, Data Use in the revoke set, no TCF answer              | unset       | unset               | unset      |
| No signal (or neutral for this Data Use)                                | set         | unset               | unset      |

With an opt-out listed after `tcf`, a request carrying both resolves unset
under every baseline for the Data Uses that opt-out revokes. An expired TCF
record resolves as the "no signal" row. Whether an unset outcome is also a
**withdrawal** is a separate, narrower question (§4.2).

### 4.2 Withdrawal vs. absence

Withdrawal (destructive, expiring the `ts-ec` cookie and writing the
identity-graph tombstone) and non-grant (the permission is simply unset, EC response
headers stripped, nothing egressed) are distinct outcomes, never conflated.
"Baseline" below means the resolved acquisition rule for
`necessary.operations.storage` in the request's jurisdiction, resolved once
at `EcContext` construction (`storage_acquisition`), never a group label.

The implemented trigger, exhaustively (nothing else withdraws):

1. **A TCF record refusing storage (Purpose 1) withdraws iff the baseline is
   not `granted`, and only when the refusal is carried by the live
   request.** Under a `requires_signal` (or `denied`) baseline the refusal
   is the visitor declining the very signal storage depends on, so it
   withdraws. Where the baseline is `granted`, storage never depended on
   the record, so the refusal suppresses use without destroying the
   identifier. Tombstones are irreversible, and PR #838 wrote them for
   visitors in unregulated jurisdictions whose global CMP emitted a
   purpose-refusing string. The `EcContext` consent pipeline runs without
   the persisted-KV inputs, so the withdrawal decision sees live request
   signals only, satisfying the draft's live-request constraint by
   construction.
2. **US-style opt-outs never withdraw.** GPC and sale opt-outs are use
   restrictions, which suppress the permissions the policy revokes (EC
   headers stripped, nothing egressed) but never trigger destruction, so
   lifting the opt-out restores the identity.
3. **A malformed record never withdraws.** It suppresses only (§4, step 2).
   Destruction requires an affirmative, decodable signal.
4. **Absence of signal never destroys identity.** A visitor who has not yet
   made a choice is never stripped of an existing identity.
5. **A policy change is not a user signal.** There are no runtime policy
   edits (§3.1), and a rebuild that tightens a baseline does not itself
   tombstone, because withdrawal still requires the affirmative refusal above on a
   live request.

The draft's additional trigger, an explicit storage-withdrawal or
authenticated deletion request honored in every jurisdiction, has no
implemented carrier, as no such endpoint exists. It is recorded as deferred
(§11), and when it arrives it joins this list as a global trigger.

A provider answers withdrawal separately from its signal, through
`withdraws` on `PermissionSignalProvider`, and core scopes any answer to the
baseline (`withdrawn` in `permission_signal/mod.rs`), carrying the result as
`PermissionState::storage_withdrawn`. Of the shipped providers only TCF
answers it. Every arm above has direct coverage in
`crates/trusted-server-adapter-axum/tests/permission_signals.rs`, with the
real providers assembled. Refusal under `requires_signal` withdraws, refusal
under `granted` does not, consent does not, GPC alone does not, sale opt-outs
do not, and no signal does not. A malformed record does not (`ec/consent.rs`),
and a deployment whose list leaves out `tcf` cannot withdraw at all.

### 4.3 Withdrawal durability (largely deferred)

Implemented behavior (`ec/finalize.rs`): when the request carries the
withdrawal signal and the client presented a cookie, the response expires
the EC cookie, and the identity-graph tombstone is written for each
presented identifier the provider accepts (the incoming cookie value and
the active value). The tombstone is the authoritative revocation marker for
subsequent EC behavior. A tombstone write failure is logged at error level
and the request completes, so the write is best effort.

The draft's durability protocol is **not implemented** and is recorded as
deferred follow-up in full: the family ID with deterministic derivation for
legacy rows, the family revocation record written before the cookie
expires, the permission-exempt suppression and authority-state records with
CAS fencing, evidence-recency comparison and anti-replay pinning, the
durable negative-intent outbox, the global identity safety breaker, and the
associated consistency and retention contracts. That machinery depends on
storage primitives (linearizable per-key CAS, independent durability
domains) the current adapters do not qualify. The 2026-07-31 draft remains
the reference design for that work. Until it lands, the known gaps the
draft called out stand. Cookie expiry is not fenced on the tombstone
commit, and revocation durability is bounded by the KV store's behavior.

### 4.4 Signal normalization

The consent subsystem (`consent/mod.rs`) remains the decoder and
normalizer. A provider reads its output as the decoded consent record on
`SignalInput`, and a provider for a scheme the pipeline does not decode reads
the request evidence instead. The implemented pipeline:

1. Extract raw signals from cookies and headers, and decode TCF v2, GPP,
   and US Privacy. A decode failure keeps the raw string and leaves the
   decoded field empty, which the permission layer reads as
   malformed-present (§4, step 2).
2. Resolve standalone-TCF vs GPP-embedded-TCF conflicts per the configured
   mode (`restrictive`, `permissive`, `newest`), preserving the pre-epic
   selection algorithm.
3. Apply the expiry check, where a TCF record older than the configured maximum
   age has its decoded form cleared, the `expired` flag set, and its raw
   string preserved. Expiry is its own state, excluded from
   malformed-present, and resolves as absent for acquisition.
4. Construct a US Privacy string from GPC for US privacy states with no
   explicit USP cookie, so the opt-out also travels in transport fields.

The draft's declared reordering, expiry filtering **before** conflict
resolution, was **not implemented**: conflict resolution still runs first,
so the pre-epic order stands. Recorded in §11.

**Persisted-KV consent.** When the pipeline runs with an EC ID and a KV
store (not on the `EcContext` construction path), a request carrying no
consent signals falls back to the consent persisted for that EC ID, with
the jurisdiction re-derived from the current request's geo. Staleness is
enforced by the store, where entries are written with a TTL equal to
`max_consent_age_days`, so an entry older than a live record's allowed age
has expired out of the store. A live signal always wins because the
fallback is consulted only when the request carries none. The draft's
declared change, running the loaded record through the full normalization
pipeline, is not implemented. The loaded record substitutes directly. The
narrow read is permission-exempt by construction, since determining
storage cannot itself require storage.

**Proxy mode.** Proxy mode still skips semantic decoding entirely. The
draft's minimal opt-out extraction was not implemented, but the fail-open
consequence the draft feared does not arise under the permission model. A
present record in proxy mode is present-but-undecoded, which blocks every
baseline grant (§4, step 2), and the GPC header needs no decoding, so the
GPC opt-out is honored directly. No grants are ever derived in proxy mode.
The net posture is equal to or more restrictive than the draft's row.
Absent records resolve to the baseline.

### 4.5 US signal field mapping

Implemented sources, as declared in the shipped `signals` section:

| Source                             | Effect                                  |
| ---------------------------------- | --------------------------------------- |
| `Sec-GPC` request header           | US-style opt-out                        |
| GPP US section sale opt-out        | US-style opt-out                        |
| US Privacy `opt_out_sale = Y`      | US-style opt-out                        |
| US Privacy `opt_out_sale = N`      | Nothing (no grant class exists for USP) |
| Any explicit Not Applicable value  | Nothing                                 |
| Absent / unknown / reserved values | Nothing                                 |

An opt-out revokes the Data Uses the policy's `revokes` value names. The
sample, `config/permissions/sample.yaml`, lists device storage, sale,
sharing, third-party sale, first- and third-party targeted advertising, and
profiling, so an opted-out visitor gets no Edge Cookie written and no
identifier shared while contextual advertising and measurement continue. That
is broader than the draft's mapping, which scoped sale opt-outs to
personalized-ad selection only, and a deployer changes it by editing the list
or writing `revokes: all`. Each source is read by its own provider (`gpc`,
`gpp_sale_opt_out`, `us_privacy`), which runs only when
`[permission_signal] provider` names it or the key is omitted, and whether an
opt-out stands over a TCF answer is the order (§4). No sale-family opt-out is
destructive (§4.2).

The remainder of the draft's §4.5 is **not implemented** and is recorded
as deferred: `SharingOptOut` and `TargetedAdvertisingOptOut` as distinct
inputs, grant-class non-opt-out values, embedded-GPC detection inside GPP
sections, the per-section applicability and aggregation algorithm with its
state-over-national precedence, the mapped-section malformed blocker at
per-section granularity, the derived OpenRTB `gpp_sid` construction with
the `__gpp_sid` consistency companion, and the complete pinned section
map. The current GPP decoder surfaces the EU TCF section, the section ID
list, and a US sale opt-out. Extending it to the full pinned registry is
its own project.

#### 4.5.1 GPP registry snapshot (deferred)

Not implemented. The vendored registry snapshot, the pinned per-section
accepted versions, the provenance manifest, and the fixture corpus travel
with the §4.5 decoder work. The 2026-07-31 draft's §4.5.1, including the
pinned upstream commit, remains the reference for that effort.

## 5. Jurisdiction resolution

### 5.1 Order

Geo resolution runs **before** permission resolution. Jurisdiction is an
input to the permission set, which is why geo providers cannot themselves
be gated on it (providers spec §5). The selected geo provider resolves a
country and optional region, and the tree is walked most specific first, being
the region node, then the country node, then the top node, matched
case-insensitively. Implemented in
`EcContext::read_from_request_resolving_geo` and
`PermissionMaps::rules_for`.

### 5.2 Lookup failure

Implemented, with the failure state carried explicitly:
`GeoStatus { Located, NoLocation, Failed }` (in `ec/consent.rs`) separates
"the provider resolved no location" from "the lookup errored". A **failed**
lookup resolves every permission to the **requires-signal floor** and its
jurisdiction to `unknown`, never the tree's top node. The failure is logged at error
level so it is visible. The storage-withdrawal baseline follows the same
floor, so a failed lookup cannot widen destructive withdrawal either. The
rule is proven through the seam by
`a_geo_provider_failure_resolves_permissions_at_the_requires_signal_floor`
in `ec/mod.rs`, which drives a `PlatformGeo` that returns an error rather
than constructing the status by hand.

**Which providers can reach it.** The floor is the contract for a geo
provider that performs its own fallible lookup, and none of the providers
shipped in this workspace is one. Fastly's `geo_lookup` returns an
`Option` and the SDK collapses every hostcall, buffer and parse failure
into `None` before it reaches the caller. The Cloudflare provider reads
request headers, which cannot error. The Axum and Spin providers resolve
nothing at all. `DisabledGeo`, the default whenever
`[geo] provider` is not `"platform"`, returns nothing by construction. So
**a host geo outage today does not reach this floor.** It surfaces as
`Ok(None)`, which is `NoLocation`, and falls back to the tree's top `group`
like any other unmatched request. A deployer
choosing a permissive top `group` is therefore choosing what a geo
outage does, and should read §5.3 and the top-node guidance with
that in mind. The floor becomes reachable when a vendor geo crate under
`crates/geo/` does a real lookup that can fail.

At the floor, an explicit valid grant still counts. A TCF record consenting
to a mapped purpose sets that permission under `requires_signal`, exactly
the divergence-from-deny-all the draft declared for this row. Absent,
malformed, or refusing evidence sets nothing.

Not implemented: a lookup-failure metric (the error log is the signal
today) and the draft's `regime = "gdpr"` component of the failure profile,
since no regime concept exists. The draft's capability check, that an
adapter whose geo implementation can never resolve anything must not accept
the selection, is part of the providers-spec provider qualification rather
than this model.

### 5.3 No geo provider selected

Every request resolves at the top of the rules tree, so jurisdiction becomes
a static constant, which is only honest when the operator can genuinely
assert single-jurisdiction traffic.

Constraint, implemented in
`GeoConfig::validate_jurisdiction_acknowledgment`: **startup fails** when
an Edge Cookie provider is configured and no geo provider is selected,
unless the operator sets `[geo] assume_single_jurisdiction = true`. This
makes the dangerous migration config (a permissive top `group` with
geo unset) an explicit operator decision rather than an accident, closing
the highest-severity finding of the PR #838 review.

The guard's consumer list is narrower than the draft's. The draft enumerated
every jurisdiction consumer (EC provider, regime-gated auction dispatch,
raw-EC and EID egress). In the implementation the EC provider is the only
consumer whose behavior the policy gates, because auction dispatch was not
migrated (§7.4), so the guard fires on the EC provider selection alone. When
dispatch joins the model, the guard's trigger list grows with it.

### 5.4 Defaults: the tree's top node plus a protective floor

The top node of the `rules:` tree is **required in every mode** and is
validated at startup, meaning it must carry a `group` naming a defined group
and a `jurisdiction`. Every node below may carry its own `jurisdiction`, and a
node that carries none inherits the nearest ancestor that does (§3.2). A no-geo
single-state deployment states that state's group at the top and nests nothing
beneath it, though `us-state` is not valid there, because the top node names no
state. The top node covers two states
the draft kept separate:

- the geo provider resolved no location (or none is configured), and
- the resolved country or region has no node of its own.

`[geo] default_country` is retired. Treating an unknown visitor as being in a
chosen country was a fiction, and the tree states the unknown case explicitly
at its top instead, with the group and the consent handling side by side. The
draft's `rules.default` policy entry does not exist either, so
"resolved-but-unmatched" falls through the parent chain to the same top node
as "unresolved". The separation the draft treated as safety-critical is the
one the implementation does keep, where a **failed** lookup never reaches the
top node and resolves at the requires-signal floor instead (§5.2).
With no top `group` or no `jurisdiction` startup fails, and in the unreachable
belt-and-braces case where resolution still finds no node, the floor
applies.

### 5.5 Policy revision activation (deferred)

Not implemented, and currently moot. Policy is compiled into the binary
(§3.1), so the deployed artifact is the policy identity and there is no
runtime activation to coordinate. The draft's activation design, covering
the JCS-canonical policy digest and ordinal pair, the activation register
and candidate protocol, fleet readiness and quiescence, admission leases,
the hash-linked activation journal with store-clock retention, and the
model-epoch transition, is the reference design for the runtime-policy
follow-up recorded in §11. Its guarantee that mixed-revision irreversible
behavior is prohibited is honored today by construction, because a
deployment runs exactly one embedded policy and destructive withdrawal
requires a live user signal (§4.2), never a policy change.

## 6. Failure-mode matrix (normative, implemented)

| Condition                                         | Resolution behavior                                                                                                                                                                                                                |
| ------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Geo lookup reports a failure                      | Requires-signal floor for every permission and an `unknown` jurisdiction, never the top-node baseline, with the error logged. No provider shipped today can report one, so a host geo outage lands on the row above instead (§5.2) |
| No geo provider configured                        | Top-node baseline, guarded by `assume_single_jurisdiction` (§5.3)                                                                                                                                                                  |
| Country resolved, no matching node                | Top-node baseline (§5.4)                                                                                                                                                                                                           |
| Region resolved, no region node                   | Country node's group, and the nearest ancestor's `jurisdiction`                                                                                                                                                                    |
| Top node missing `group` or `jurisdiction`        | Startup failure (§3.3)                                                                                                                                                                                                             |
| EC provider configured, no geo, no acknowledgment | Startup failure (§5.3)                                                                                                                                                                                                             |
| Malformed `permissions.yaml`                      | Parse error at settings load, once per instance, because the embedded file is a build-time constant, never per-request                                                                                                             |
| Undecodable record present (TCF, GPP, or USP)     | Revokes every Data Use (fail-closed acquisition), never withdraws, and still honors opt-outs                                                                                                                                       |
| Expired TCF record                                | Distinct state, not malformed, and treated as absent, so the baseline applies                                                                                                                                                      |
| Signals contradict (opt-out plus consent)         | The later provider in the configured order decides, so under the default order a TCF consent amends an earlier opt-out (§4)                                                                                                        |
| No EC provider selected                           | Identity fails closed, with nothing created and an incoming cookie value never used or egressed (§7)                                                                                                                               |

The posture is fail-closed. Every ambiguous state resolves to the
configured baseline or more restrictive.

## 7. Enforcement points

Consumers of the resolved set in the implementation:

1. **EC provider execution.** The provider declares
   `required_permissions()`, core resolves a `PermissionState` once per
   request at `EcContext` construction, and the provider executes only when
   every declared permission is set (`ec_allowed`). A provider that
   requires nothing always runs. **Geo** is ungated because gating it is
   circular, jurisdiction being an input to permission resolution.
   **Device** is ungated by a separate, deliberate decision, because its
   security-classification role must run for traffic that has granted
   nothing, and operator selection is the recorded authorization (providers
   spec §5) for the built-in and host providers, which declare nothing. A
   device provider an integration module supplies may not declare a
   permission at all: a nonempty declaration is refused at startup, naming
   the module and the Data Uses, because no per-request device gate exists
   to honor it (registry `resolve_device_provider`). The built-in Edge
   Cookie providers declare `necessary.operations.storage`.

2. **EC lifecycle.** Creation requires the provider's declared permissions
   through the gate above. Withdrawal follows §4.2. Recognition and
   revocation of an existing identifier are never permission-gated, and the
   withdrawal path runs precisely when `ec_allowed` is false, reading the
   raw cookie value kept for that purpose.

3. **Sharing beyond the edge.** One predicate,
   `EcContext::ec_sharing_allowed`, requires the provider gate plus
   **both** `necessary.operations.storage` and
   `advertising_marketing.first_party.targeted`. A storage-only grant
   therefore keeps first-party use while withholding partner sharing. The
   implemented inventory:

   | Path                                         | Gate                                                          |
   | -------------------------------------------- | ------------------------------------------------------------- |
   | Bidstream EIDs (every auction path)          | `gate_eids_by_permissions`, storage plus personalized ads     |
   | OpenRTB `user.id` on the `/auction` endpoint | `ec_sharing_allowed`                                          |
   | Identify endpoint (partner-facing)           | `ec_sharing_allowed`                                          |
   | Pull sync (browser-request-scoped)           | `ec_sharing_allowed`, from the live request resolution        |
   | Batch sync (context-free S2S)                | Authenticated, and withdrawn or missing rows are ineligible   |
   | KV EID resolution for auctions               | `ec_allowed`, then the EID pair gate on the result            |
   | Publisher navigation and page-bids `user.id` | `ec_sharing_allowed` (the storage plus personalised-ads pair) |

   With **no EC provider configured**, identity fails closed, meaning the gate is
   closed rather than open by default (`ec_allowed` is false), so a cookie
   value present on the request is treated as absent and never used or
   egressed. This replaces PR #838's vacuously-true `is_none_or` check.

   **Follow-up note (recorded, not silent).** The publisher navigation and
   page-bids paths attach the EC-derived request ID and `user.id` under
   the provider gate (`ec_allowed`) rather than the sharing pair, while
   their EIDs are pair-gated. With the built-in providers (storage-only
   requirement), a storage-only grant can therefore still place the EC in
   `user.id` on those paths. Aligning them with the `/auction` endpoint's
   pair gate is recorded follow-up (§11). The draft's fuller inventory
   (proxy/click/Testlight forwarding gates, the observability denylist
   with typed redaction boundaries, the raw-regulatory-transport
   destination allowlist, integration response cookies) is deferred with
   it. Today EC values are truncated (`log_id`) before logging as the
   observability mitigation.

4. **Server-side auction dispatch (not migrated).** Dispatch is still gated
   by the consent subsystem (`consent_allows_server_side_auction`), not by
   the permission model. When the jurisdiction is GDPR or unknown, or an EU
   TCF signal is present, dispatch requires an effective TCF record
   consenting to Purpose 1, and otherwise no bid request leaves (a no-bid
   response, with no PBS/APS call and no UA/IP/geo forwarding). Known
   non-GDPR jurisdictions without an EU TCF signal dispatch freely. The
   draft's regime-keyed dispatch table, and the `ContextualAuctionView`
   positive projection for dispatch with personalized-ad selection unset,
   are **not implemented**. When personalized-ad selection is unset today,
   EIDs and the pair-gated identifiers are stripped but dispatch is the
   ordinary request, not a contextual projection. Recorded as deferred
   (§11) together with §3.4.

The client-cycle resolve endpoint (`/_ts/api/v1/ec/resolve`) is a further
consumer, where a provider-derived identifier posted by the page is accepted only
through the same provider and permission gates.

### 7.1 Contextual OpenRTB v1 allowlist (deferred)

Not implemented. The machine-readable projection manifest, its path
grammar, cardinalities, cross-field rules, and derivation vocabulary, and
the conformance walker over final encoded bytes, belong to the dispatch
migration (§7.4) and remain specified by the 2026-07-31 draft for that
work.

## 8. Testing strategy

Implemented, in `permissions.rs`, `permission_signal/mod.rs`,
`ec/consent.rs`, `ec/mod.rs`, `ec/finalize.rs`, `consent/mod.rs`, the four
provider crates, and `crates/trusted-server-adapter-axum/tests/permission_signals.rs`:

- **Signal order.** The last provider with an opinion decides, silence
  leaves an earlier answer standing, and silence from every provider is not
  a refusal. One test per opt-out source shows a TCF answer applied over it
  under the default order (`a_prompt_answer_applies_over_a_gpc_signal` and
  companions), and `the_opt_out_wins_when_a_deployment_puts_it_last` shows
  the reverse. An omitted `provider` runs every provider in the offered order,
  a configured list is the order they run in, an empty list is accepted, and
  an unknown or repeated name is refused.
- **Withdrawal scoping.** One test per §4.2 arm: refusal under
  `requires_signal` withdraws, refusal under `granted` suppresses without
  destroying, consent is not a withdrawal, GPC alone never withdraws, sale
  opt-outs never withdraw, no signal never withdraws, malformed never
  withdraws.
- **Fail-closed acquisition.** Malformed records block baseline grants,
  each undecodable record family is detected, and an expired TCF record is not
  treated as malformed and resolves at the baseline.
- **Geo status.** A failed lookup resolves at the requires-signal floor
  (permissions and the storage baseline both), driven through a
  `PlatformGeo` that returns an error rather than from a hand-built status,
  and no-location falls back to the configured default.
- **Policy parsing and validation.** Groups, rules, detailed-rule
  acquisition maps (including `requires_signal` per rule), rejection of
  unknown groups, unknown permissions, unknown acquisitions, incomplete
  groups, case-insensitive duplicate rule keys, unknown revoke keywords,
  and the signals-section round trip.
- **Shipped-table coverage.** All 30 EU/EEA codes lock storage to
  requires-signal (§3.5).
- **Vocabulary breadth.** A TCF record grants or revokes every one of the
  eleven mapped purposes, not only storage and personalized ads.
- **Gates.** Provider execution blocked without its required permissions,
  the sharing pair withheld on a storage-only grant, EIDs stripped when
  either permission of the pair is unset, and the no-provider stateless
  posture.

The draft's fuller matrices (the complete normalization matrix as
table-driven tests, the per-row egress inventory with a denylist check, the
dispatch regime-by-signal matrix, contextual-serializer poisoning, S2S
authority denial reasons, and the §4.3 fault-injection suite) travel with
their deferred features.

## 9. Out of scope

- The §4.3 durability protocol, §4.5 full field mapping and registry
  snapshot, §5.5 activation apparatus, §7.4 dispatch migration, and §7.1
  contextual projection: deferred follow-ups, recorded in §11 with the
  2026-07-31 draft as their reference design. Deferred, not rejected.
- Runtime policy configuration (a `[permissions]` config section):
  deferred until the config push and activation pipeline exists (§3.1).
- Per-signal jurisdiction scoping (honoring GPC only where a law defines
  it): rejected, as in the draft. The opt-out signal layer is
  jurisdiction-free in the implementation, and the country baseline decides
  only what a revoke has to drop.
- An authenticated deletion or explicit storage-withdrawal endpoint:
  deferred (§4.2).

## 10. Divergences from issue #779

This spec supersedes #779 on the following points, so there is one
acceptance contract, not two:

| #779 says                                                                                    | This spec says                                                                                                                                                                                                          | Why                                                                                                                                                                          |
| -------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Unmatched countries fall to `default_country`                                                | Adopted, with a changed mechanism since 2026-09-01. Unmatched and unresolved requests both fall to the required top node of the `rules:` tree, and a **failed** lookup floors instead (§5, §12)                         | The failure state is the one that must never reach a permissive default, and the draft's `rules.default` split was not kept                                                  |
| The full TCF purpose vocabulary is modeled                                                   | Adopted and extended. All eleven purposes are signal-resolved, and the full Privacy Taxonomy is carried as declared baseline (§2)                                                                                       | The joint taxonomy work made whole-taxonomy declaration the goal, and `denied` defaults keep undeclared uses inert                                                           |
| Policy is an embedded file                                                                   | Adopted. `permissions.yaml` is compiled into the build (§3.1), and runtime configuration is deferred follow-up                                                                                                          | The runtime push and activation pipeline does not exist, and version control is the audit trail meanwhile                                                                    |
| Permission sources are open-ended (#777: publisher interaction, external services may grant) | Adopted as a seam. A source is a permission signal provider, a crate implementing `PermissionSignalProvider`, registered by an integration builder and asked in the order `[permission_signal] provider` gives (§1, §4) | The four shipped schemes already use it from outside core, so the interface has real consumers, and publisher interaction or an external service arrives as another provider |

## 11. Revision record vs the 2026-07-31 draft

One row per divergence between the draft and the implementation this
revision was verified against (branch `split/5-response-hook-docs`,
PR #1045).

| Draft position                                                                                                                                            | Implemented position                                                                                                                                                                                                                                                                  | Why                                                                                                                                                                                           |
| --------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Vocabulary is two TCF-purpose identifiers, enforced permissions only (§2)                                                                                 | IAB Privacy Taxonomy Data Uses, with eleven named purposes all signal-resolved, plus 53 taxonomy Data Uses carried as declared but unenforced baseline flags                                                                                                                          | The joint taxonomy adoption postdates the draft, and whole-taxonomy declaration serves completeness and demonstration, with `denied` defaults keeping unenforced flags inert                  |
| Policy lives in `[permissions]` in `trusted-server.toml`, published via `ts config push` (§3.1)                                                           | Policy is `permissions.yaml`, compiled into the build with `include_str!`, parsed once and covered by tests                                                                                                                                                                           | The runtime config push and activation apparatus does not exist, so publishing runtime policy without it would recreate the hazards the draft cataloged. Runtime policy is deferred follow-up |
| Every group carries a required `regime` class read by auction dispatch (§3.2)                                                                             | No regime field exists                                                                                                                                                                                                                                                                | Its only consumer, regime-gated dispatch, was not migrated, and the field returns with that work                                                                                              |
| Overrides name explicit acquisition rules, replacing the `+`/`-` sigils (§3.2)                                                                            | Adopted. A detailed rule's `permissions` map assigns `granted`, `requires_signal`, or `denied` per Data Use                                                                                                                                                                           | The draft's requirement, expressed in the YAML schema. `requires_signal` is now expressible per rule                                                                                          |
| Two fallbacks: policy `rules.default` for unmatched countries, `default_country` only for the static no-geo mode (§5.4)                                   | One required fallback covers unmatched and unresolved requests in every mode, startup-validated, and a failed lookup floors separately. It was `[geo] default_country` until 2026-09-01 and is now the top node of the `rules:` tree (§12)                                            | One deployer knob is simpler, and the safety-critical separation kept is failure vs absence, carried by `GeoStatus`                                                                           |
| Validation checks assigned ISO 3166-1 codes, assigned subdivisions, and a group identifier grammar (§3.3)                                                 | Validation covers unknown groups, permissions, acquisitions, revoke rules, incomplete groups, case-duplicate rule keys, and unknown fields on detailed rules                                                                                                                          | Smaller surface shipped first, and the EU/EEA coverage test guards the shipped table against the typo class. ISO-assignment checks are future hardening                                       |
| Three-class signal taxonomy with regime-scoped grant acceptance, where the US posture is `requires_signal` with GPP/USP non-opt-out values as grants (§4) | Of the shipped providers TCF is the only grant source, the US posture is a `granted` baseline that opt-outs revoke, and explicit non-opt-out values grant nothing                                                                                                                     | A simpler two-signal model without regimes. The cost, no-signal US traffic is allowed by baseline rather than blocked pending a signal, is a deliberate policy choice in the shipped file     |
| Malformed-present blocks grants per record family and mapped section (§4.4, §4.5)                                                                         | Any present-but-undecodable record revokes every Data Use for the request                                                                                                                                                                                                             | Strictly more restrictive simplification, and per-family scoping needs the full §4.5 decoder work                                                                                             |
| Normalization runs expiry before conflict resolution, a declared change (§4.4)                                                                            | Conflict resolution still runs before the expiry check                                                                                                                                                                                                                                | The reordering was not implemented, though the expired state itself (distinct from malformed, absent for acquisition) was adopted                                                             |
| Persisted-KV consent flows through the full normalization pipeline with an explicit TTL comparison (§4.4)                                                 | The loaded record substitutes directly when the request carries no signals, jurisdiction re-derived, and staleness is enforced by the store TTL (`max_consent_age_days`)                                                                                                              | The store-level TTL delivers the staleness bound without a second normalization pass                                                                                                          |
| Proxy mode gains minimal opt-out extraction (§4.4)                                                                                                        | Proxy mode still skips decoding, so a present record blocks all grants via the malformed-present rule and the GPC header opt-out is honored without decoding                                                                                                                          | The permission-layer outcome is equally or more restrictive with no new decode paths, and this is revisited with the §4.5 decoder work                                                        |
| Withdrawal has four triggers including an explicit storage-withdrawal or authenticated deletion request (§4.2)                                            | The TCF Purpose 1 refusal under a non-granted baseline is the only trigger, and opt-outs, malformed records, absence, and policy changes never withdraw (adopted)                                                                                                                     | No deletion endpoint exists to carry the extra trigger, so the narrowest destructive surface shipped first                                                                                    |
| §4.3 durability protocol: family records first, suppression and authority-state records, outbox, breaker, strong reads                                    | Cookie expiry plus best-effort identity-graph tombstones per presented identifier, with failures logged                                                                                                                                                                               | The protocol requires storage primitives (linearizable CAS, independent durability domains) the adapters do not yet qualify, so it is deferred with the providers-spec storage work           |
| §4.5 field mapping and §4.5.1 vendored registry snapshot (sharing/targeted opt-outs, embedded GPC, applicability, derived `gpp_sid`)                      | Opt-out sources are the GPC header, a GPP sale opt-out, and a USP sale opt-out. The revoke set is policy-declared, and the shipped file lists device storage, sale, sharing, third-party sale, targeted advertising and profiling (§4.5)                                              | The full decoder and registry vendoring are their own project, and the policy-declared revoke set gives deployers the scoping lever meanwhile                                                 |
| §5.5 activation: JCS policy digests, ordinals, activation register, journal, drains, admission leases                                                     | None of it exists, and the built binary is the policy identity                                                                                                                                                                                                                        | With no runtime policy there is nothing to activate, and the draft remains the reference design for the runtime-config follow-up                                                              |
| §3.4 single jurisdiction truth, and §7 dispatch gated on the policy regime with a contextual projection                                                   | Half adopted. Since 2026-09-01 the `rules:` tree carries regime applicability and the two `[consent]` lists retire into it (§3.4), while auction dispatch still keeps the consent-subsystem gate (effective TCF Purpose 1 for GDPR or unknown jurisdictions), with no contextual view | Dispatch migration is follow-up. The legacy-list drift risk the draft named still stands and is recorded rather than resolved                                                                 |
| Every raw-EC egress path is pair-gated, with per-row tests and a denylist check (§7)                                                                      | Pair gating is centralized in `ec_sharing_allowed` (auction endpoint `user.id`, publisher navigation and page-bids `user.id`, identify, pull sync) and `gate_eids_by_permissions` (EIDs everywhere). Batch sync checks row state only                                                 | Partial adoption. Aligning the remaining paths, the S2S stored-provenance authority, and the inventory tests is recorded follow-up                                                            |
| Identity rows never store raw consent strings, only normalized provenance and a digest (§1)                                                               | The identity-graph entry stores the raw TCF and GPP strings with the row                                                                                                                                                                                                              | The normalized provenance schema belongs to the providers-spec storage work, and until then rows carry the raw strings                                                                        |
| No signals block in policy, and the signal mapping is fixed in the spec                                                                                   | New. A `signals` section in `permissions.yaml` declares the opt-out sources and revoke set, with `tcf.authoritative` governing only TCF's own effect. The TCF purpose map has since moved into the TCF provider crate (§13)                                                           | Moves signal policy from code into deployer-editable data, while what a scheme's own signal means stays with that scheme's provider                                                           |
| The §5.3 no-geo guard covers every jurisdiction consumer                                                                                                  | The guard fires when an Edge Cookie provider is configured with no geo provider                                                                                                                                                                                                       | The EC provider is the only policy-gated consumer today, and the trigger list grows when dispatch and further egress paths join the model                                                     |
| `default_country` is required only in the acknowledged static no-geo mode (§5.4)                                                                          | Required always and startup-validated. Since 2026-09-01 the requirement sits on the `rules:` tree's top node, which must carry a `group` and a `jurisdiction` (§12)                                                                                                                   | It is the baseline for unmatched requests in every mode, so it must always exist                                                                                                              |

## 12. Revision record: the rules tree (2026-09-01)

The flat rule keys and `[geo] default_country` are replaced by a single
`rules:` tree, following review discussion on PR #1084. The tree was extended
the same day, where `jurisdiction` became a per-node inherited attribute and
the two `[consent]` applicability lists retired into the tree. One row per
change.

| Before                                                                                                           | After                                                                                                                                                                                                   | Why                                                                                                                                                                                             |
| ---------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Flat rule keys, a bare country (`FR`) or a slash-joined pair (`US/CA`)                                           | One tree, with country codes at the first level and region codes nested beneath them, all matched case-insensitively                                                                                    | The nesting shows the reader the precedence instead of hiding it inside a joined key, and the file's audience is a policy owner rather than a developer                                         |
| A rule value is a group name, or a mapping of `{group, permissions}`                                             | Every node has a `group` and optional children. A single string is shorthand for that group with no children, and a mapping must carry `group:`                                                         | One node shape reads the same at every depth, and the shorthand keeps a one-line country rule to one line                                                                                       |
| `[geo] default_country` in `trusted-server.toml` names the rule for unmatched requests                           | The tree's top node carries the `group` for a request whose country cannot be resolved                                                                                                                  | Treating an unknown visitor as being in a chosen country was a fiction, and the tree states the unknown case explicitly where a reader will look for it                                         |
| The consent handling for an unresolved request follows from the chosen default country                           | The top node requires `jurisdiction:` beside `group:`, with the values of the `Jurisdiction` type (§3.2)                                                                                                | The unknown case deserves its own stated answer rather than one inherited from a stand-in country                                                                                               |
| Startup rejects a missing or unresolvable `default_country`                                                      | Startup rejects a top node missing `group` or `jurisdiction`, and reserved keys cannot collide with place codes because ISO codes are short                                                             | The same loud failure, moved to where the policy now lives                                                                                                                                      |
| `jurisdiction` is written at the top of the tree only                                                            | Any node may carry `jurisdiction:` beside `group:`, and a node carrying none inherits the nearest ancestor that does. The top node must carry both, so the inheritance always terminates                | A country or a state often needs its own regime, and inheritance states the common case once while leaving the exceptions visible where a reader meets them                                     |
| The US-state value carries the state code, as `us-state/<CODE>`                                                  | The value is bare `us-state`, valid only on a region node under `US`, and rejected at the top and on a country node                                                                                     | The matched node already names the state, so repeating the code in the value invites the two disagreeing                                                                                        |
| `consent.gdpr.applies_in` and `consent.us_states.privacy_states` in the operator TOML carry regime applicability | Both retire into the tree, where the shipped file holds the same 31 GDPR countries inheriting `gdpr` and the same 20 privacy-law states as region children of `US` with `jurisdiction: us-state` (§3.4) | One document carries the baselines and the applicability together, so a policy owner keeps one list rather than two in step, and the drift risk §3.4 recorded has nothing left to drift against |

This revision leaves unchanged the group vocabulary and flags, the signal
precedence of §4, and the requires-signal floor for a failed geo lookup, which
stays distinct from having no location. Trusted Server still encodes no
jurisdiction's law, as the deployer states the policy and the software carries
it out.

## 13. Revision record: permission signal providers (2026-09-14)

Signal precedence moves from a fixed rule in code to the order a deployment
configures, and the four shipped schemes move out of core into provider
crates. One row per change.

| Before                                                                                                                 | After                                                                                                                                                                                                                                                                                    | Why                                                                                                                                                                                                                    |
| ---------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Precedence fixed in code, most restrictive first, so an opt-out always beat a consenting TCF record                    | The order in `[permission_signal] provider`, where the last provider with an opinion decides, validated and logged at startup (§4)                                                                                                                                                       | Which scheme wins is a question about a jurisdiction and a publisher, and a fixed rule answers it in code for every deployment                                                                                         |
| A GPC header suppressed a Data Use that a TCF record consented to                                                      | The default order is `gpc`, `gpp_sale_opt_out`, `us_privacy`, `tcf`, so a TCF answer amends an earlier opt-out, and listing the opt-out after `tcf` restores the previous outcome                                                                                                        | Global Privacy Control is a browser setting with no interface of its own, and the other three carry an answer a person gave through an interface, so by default that answer amends the header the visitor arrived with |
| Signals resolved inside core through a per-permission `ConsentSignal` closure, with further sources deferred (§1, §10) | A `PermissionSignalProvider` trait in core, with GPC, the GPP sale opt-out, US Privacy and TCF each a crate under `crates/permission-signal/` that core does not name, registered through an integration builder                                                                         | A scheme core has never heard of plugs in without a change to core, and none of the four is privileged by being built in                                                                                               |
| The TCF purpose to Data Use map in the `signals` section of `permissions.yaml` (§3.2)                                  | The map in the TCF provider crate, `crates/permission-signal/tcf/src/mapping.rs`                                                                                                                                                                                                         | A deployment that runs no TCF carries no table of another scheme's numbers                                                                                                                                             |
| Malformed-present as step 3 of the fixed order                                                                         | Ahead of every provider and not configurable (§4, step 2)                                                                                                                                                                                                                                | It is error handling rather than a signaling scheme, and a place in the order would let a readable record from one scheme overwrite the refusal an unreadable record from another caused                               |
| Withdrawal decided in core from the TCF record                                                                         | A `withdraws` answer on the provider trait, scoped by core to the storage baseline, and given today only by TCF (§4.2)                                                                                                                                                                   | Revoking and withdrawing stay different questions, and a new scheme can carry its own withdrawal                                                                                                                       |
| The permission state carries the resolved set only                                                                     | It also carries `tdls`, the terms documents the configured providers declare, in provider order with duplicates removed, and the page receives `{"set":[],"tdls":[]}`. Each entry must address a document never edited once published, which the `Tdl` type documents and cannot enforce | A recipient needs the exact terms the data is available under, and an empty list says no terms were declared rather than that anything is permitted                                                                    |
| Opt-out-beats-TCF pinning tests                                                                                        | Order tests in both directions, per-source tests, and selection tests (§8)                                                                                                                                                                                                               | The tests pin the configured behavior rather than the retired rule                                                                                                                                                     |
