# Design Spec: Jurisdiction Permission Model

**Status:** Proposed. The implementation is on the branch
`split/3-permissions`, which is row 4 of the stack of draft pull requests that
builds this design. The stack starts at the pull request that carries the
design documents (IABTechLab/trusted-server#1342), and the draft for row 4
(IABTechLab/trusted-server#1345) is not yet merged to main. The pull request
that carried the implementation before (#1045) was closed on 2026-10-15
without merging. Revised against that implementation, 2026-08-25. Revised
again on 2026-09-01 for the `rules:` tree, which replaces the flat rule keys
and retires `[geo] default_country` (§3.2, §5.4, and the §12 record). Revised
again on 2026-09-14 for permission signal modules, which replace the fixed
signal precedence with a configured order (§4, and the §13 record). Revised on
2026-10-06 to say module for what an operator selects, and on 2026-10-07 to
name a module by its crate folder, so the selector reads
`[permission-signal] modules`. Revised on 2026-10-15 after a review of this
page against the code at `split/3-permissions` (see the Review log), so that
§1 to §8 and the records in §10 to §13 state what that code does.
**Author:** Engineering
**Issue references:** #779
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-provider-migration-rollout-design.md`
**Last updated:** 2026-10-15

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
through this model are EC module execution, EC creation and withdrawal, EID
transmission into the bidstream, and sharing of the EC identifier beyond the
edge (§7). Server-side auction dispatch is not yet a consumer (§7.4).

The set is resolved from three inputs:

1. **Jurisdiction**, the country and optional region the request resolves to
   (§5).
2. **Policy**, a declarative map from jurisdiction to a baseline acquisition
   rule per permission, plus a declared signal policy (§3).
3. **Signals**, answered by the permission signal modules a deployment
   configures, in the order it configures (§4). Five implementations ship,
   being `gpc`, `gpp`, `us-privacy`, `tcf` and `mtm`, each a crate under
   `crates/permission-signal/`. The first four read the `Sec-GPC` header and the
   IAB GPP, US Privacy and TCF strings, and `mtm` reads the answer of the
   Preference Management Platform (PMP) under the Model Terms for Marketing
   (§4.6).

Issues #777 and #779 also envision publisher interaction and external services
as permission sources. The module seam is that source interface. A module
is a crate implementing `PermissionSignalModule`
(`crates/trusted-server-core/src/permission_signal/mod.rs`) that answers
`Grant`, `Revoke` or `Neutral` for each permission, so a new source is a new
module rather than a new resolution algorithm. Each adapter links the module
crates and lists them, in the default order, in its own
`shipped_signal_modules()` function, and `build_permission_signal_modules` in
core selects and orders them from `[permission-signal] modules` (§4, §4.7). A
module is not an integration, so none of the five crates registers through the
integration registry, ships browser JavaScript, or is selected in a page
integration's section.

Core is not silent about the schemes. Its consent pipeline (`consent/`) decodes
TCF, GPP and US Privacy strings, and the policy vocabulary in `permissions.rs`
names the opt-out sources (`gpc`, `gpp_sale_opt_out` and `us_privacy_opt_out`)
and the `signals.tcf` block. What each scheme says about each Data Use, such as
the TCF purpose map, lives in the module crate for that scheme, and core
answers for no scheme ahead of the modules (§4). §10 and §13 record the change.

Scope. The model governs decisions Trusted Server makes. A downstream protocol
receives the full regulatory context only where that protocol defines fields
for it, being the OpenRTB consent fields, which carry only the raw strings a
signal module vouched for (§4.4). The draft's stronger rule, that identity rows
carry normalized per-permission provenance and a digest and never a raw string,
is **not yet true**, because the identity-graph entry (`KvEntry` in
`ec/kv_types.rs`) stores the raw TCF and GPP strings that a module vouched for
alongside the row today. The normalized provenance model travels with the
providers-spec storage work and is recorded as deferred (§11).

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
  Data Use identifier. All eleven are resolved against the session signals
  (§4). A present TCF record grants or revokes each mapped purpose, and `mtm`
  answers for all eleven when the PMP cookie is present (§4.6). Two
  purposes have no published Data Use yet, so purpose 1 uses a proposed
  `necessary.operations.storage` key and purpose 11 keeps its TCF identifier
  `select-basic-content`, both flagged for an upstream taxonomy addition.
- **Fifty-three additional taxonomy Data Uses**, carried so `permissions.yaml`
  can declare a policy flag for the whole taxonomy. No module gates on them
  today, and a signal reaches only some of them. The TCF module maps eight of
  them to a TCF purpose (`crates/permission-signal/tcf/src/mapping.rs`), and
  the opt-out list in the sample policy names four (`disclosure.sale`,
  `disclosure.share`, `disclosure.third_party_sale` and
  `advertising_marketing.third_party.targeted`), one of which TCF also maps. A
  signal therefore reaches eleven of the fifty-three under the sample policy,
  and the other forty-two keep their configured baseline. They exist for
  completeness, testing, and demonstration, and where no informed policy
  decision has been made the shipped file sets them `denied`.

The two Data Uses that carry enforcement weight today are
`necessary.operations.storage` (TCF Purpose 1, storage) and
`advertising_marketing.first_party.targeted` (TCF Purpose 4, personalized-ad
selection). Module execution gates on whatever a module declares (the
built-in Edge Cookie modules declare storage), and sharing beyond the edge
gates on the storage plus personalized-ad pair (§7).

This is a deliberate departure from the draft's rule that a permission appears
only when it has both a signal mapping and an enforcement point. The eleven
named Data Uses all have the signal mapping, and the fifty-three further
Data Uses are declared policy rather than enforced policy, as no module gates
on them. The file header of `permissions.yaml` states this plainly, and the
`denied` default means an undeployed flag cannot silently authorize anything.
Policy validation still **rejects** any rule or flag that references an
identifier outside the modeled vocabulary, so a policy cannot name a Data Use
the code does not know.

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

Policy lives in a human-editable permissions YAML document, which this spec
calls `permissions.yaml` after the name the code and the guides use. The
repository file the build compiles in is `config/permissions/sample.yaml`, a
sample for testing and evaluation only, and no other file or setting supplies
policy on this row. The build embeds the file with `include_str!`, and the
embedded text is parsed once per instance (cached behind a `OnceLock` in
`PermissionMaps::standard`). A deployer edits or replaces the file and
rebuilds to change policy. The file is not read at runtime.

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
- The draft's complaint about unknown fields is only partly answered, because
  the parser does not reject unknown keys as such. Any key beside `group`,
  `jurisdiction` and `permissions` in a node of the `rules:` tree is read as a
  place code, since the tree has no other way to name a child. Under a region
  that is refused as a place written under a region. At the top and under a
  country, a misspelled key fails only when its value is neither the name of a
  defined group nor a block carrying `group:`, and otherwise it becomes a place
  that no request matches. A key outside `groups`, `rules` and `signals` is
  ignored, and so is a key inside `signals`, `signals.tcf` or
  `signals.us_opt_out` that the parser does not define. No test covers a
  misspelled key (Open questions, item 3).
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
  region node and is rejected at the top and on a country node. The parser does
  not check the country above the region, so `us-state` is also accepted on a
  region under a country other than `US`, and names that region's code (Open
  questions, item 4).
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
  record's own grants and revokes apply, and not whether a record refusing
  storage withdraws (§4.2). The TCF purpose to Data Use mapping
  is not in the file. It lives in the TCF module crate
  (`crates/permission-signal/tcf/src/mapping.rs`), so a deployment that runs
  no TCF carries no table of another scheme's numbers. Nor is the order the
  signals are asked in, which is `[permission-signal] modules` in
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
  caller, the file is rejected for malformed YAML, a node that is neither a
  group name nor a block with a `group:` line, a rule referencing an
  undefined group, an unknown Data Use identifier anywhere (group flag,
  detailed-rule entry, or revoke list), an acquisition
  value outside `granted | requires_signal | denied`, a group without
  `default` that does not list every permission, duplicate rule keys under
  case-insensitive comparison (`us` and `US`), a `jurisdiction` value the
  `Jurisdiction` type cannot represent, `us-state` on the top node or a
  country node, a place written under a region, and a `revokes` keyword other
  than `all`. An unknown key is not rejected (§3.1).
- **At startup**, the top node of the compiled `permissions.yaml` must carry
  both a `group` naming a defined group and a `jurisdiction`, and the no-geo
  acknowledgment must be present where required (§5.3). A file whose top node
  is missing either key is rejected exactly as a missing
  `[geo] default_country` was rejected before. Both are settings-construction
  failures, never per-request failures. A name in `[permission-signal] modules`
  that the adapter does not link, or a name given twice, fails when the
  adapter builds its application state (§4), and the adapter then serves its
  startup error router in place of the application. Settings validation
  cannot check the names, because only an adapter knows which crates it links,
  and the `ts` command line tool links none of them (§6.1).

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
to the top node's group. The shipped table maps the EU 27 and EEA to
`gdpr-eu`, the UK to `gdpr-uk` (storage `granted`, a baseline that row 4 of the
rollout design's behavior-preservation matrix, §2, flags for the task force to
confirm with its citation, everything else opt-in), and the US and Australia to
`us-opt-out`. Countries with no node of their own fall to the tree's top
`group` (§5.4).

## 4. Signal precedence (normative, implemented)

Precedence is the **order a deployment configures**, not a rule in code. The
policy file decides what the shipped schemes mean for the deployment (§3.2),
each permission signal module decides what its own scheme says, and
`[permission-signal] modules` in `trusted-server.toml` decides the order the
modules are asked in. Each permission resolves in these steps.

1. **Policy `denied`** is never set, regardless of any signal. (Enforced in
   the resolver, `PermissionMaps::resolve_with`.)
2. **The configured modules are asked in order** (`combine` in
   `crates/trusted-server-core/src/permission_signal/mod.rs`). Each sees the
   baseline and what the modules before it settled on, and answers `Grant`,
   `Revoke` or `Neutral` for the permission. A `Neutral` answer leaves the
   prior value standing and any other answer replaces it, so **the last
   module with an opinion decides**. Every module is asked, because a
   later one may amend what an earlier one settled.
3. **No module with an opinion leaves the baseline standing**, so `granted`
   sets the permission and `requires_signal` leaves it unset. A `Revoke` drops
   a `granted` baseline, and only a `Grant` sets a `requires_signal` one.

**Core answers nothing ahead of the modules.** No step revokes every
permission because some consent record could not be decoded, and no part of
the order is fixed where a deployment cannot change it (`assemble_permissions`
in `crates/trusted-server-core/src/ec/consent.rs`, and the test
`core_answers_nothing_ahead_of_the_modules` beside it). A string that arrived
and could not be decoded is answered by the module for its own scheme. Each
shipped module reads such a string as the refusal or opt-out the string may
have carried, and only for the Data Uses its scheme covers.

- `tcf` answers `Revoke` for the 19 Data Uses a TCF purpose maps to, being the
  eleven named ones and eight further ones (§2), and only while
  `signals.tcf.authoritative` is true.
- `gpp` and `us-privacy` answer `Revoke` for each Data Use in the policy's
  opt-out revoke set (seven in the sample policy), and only while the policy
  lists their source in `signals.us_opt_out.sources`.
- `gpc` and `mtm` decode nothing. A `__mtm_pref` value that is not one of the
  three words `mtm` knows is read as absent.

A later module overrides an earlier one, as it does for any answer. Under the
default order a readable TC string that consents to storage therefore sets
storage over an unreadable GPP string, and a deployment that leaves `tcf` out
of the list removes the effect of an unreadable TC string. The Axum tests
`an_unreadable_tcf_record_is_the_tcf_modules_refusal` and
`an_unreadable_gpp_string_is_the_gpp_modules_opt_out_and_the_order_decides`
(`crates/trusted-server-adapter-axum/tests/permission_signals.rs`) run the real
modules for both. An unreadable string never withdraws (§4.2), and no module
vouches for it, so it reaches neither the page nor a bid request (§4.4). An
**expired** TCF record is deliberately a distinct state, not an unreadable one.
The pipeline clears the decoded record and sets the `expired` flag, the `tcf`
module answers `Neutral`, and acquisition proceeds as if the record were
absent, so the baseline applies.

The order is set by `[permission-signal] modules`, which takes a list rather
than a single name because several permission signal modules run, and the
list is the order they run in:

```toml
[permission-signal]
modules = ["gpc", "gpp", "us-privacy", "tcf", "mtm"]
```

- **Omitted**, every module the adapter links runs, in the order the adapter
  offers them, so a signal is never ignored because nobody listed it.
- **An empty list** runs no module, and every permission stays at its
  country and region baseline.
- **A name** is the module's crate folder below `crates/permission-signal`,
  such as `gpp`, and may also be written in full, as `permission-signal.gpp`.
- **A name the adapter does not link, or a name given twice**, fails when the
  adapter builds its application state (§3.3). The message for an unknown name
  lists the modules that are available.
- The modules acted on are written to the log once at startup, and a warning
  lists any module the list leaves out. Open questions, item 1, concerns how
  that warning compares names.

The section takes `modules` and nothing else. A block named for a module,
`[permission-signal.<name>]`, is refused today as an unknown field, because
none of the five modules has a setting to carry. A module that gained a
setting would take such a block, under the rule in the providers spec §2.1. The
earlier table name `[permission_signal]` and the earlier keys `sources` and
`module` are refused with a message that names `[permission-signal] modules`
(§6.1).

Every adapter offers the five shipped modules in the same default order,
being `gpc`, `gpp`, `us-privacy`, `tcf` and `mtm` (§4.7). Global Privacy
Control is a browser setting with no interface of its own, so it is asked
first, and the schemes that carry an answer a person gave through an interface
are asked after it. Under that default a TCF record consenting to a mapped
purpose sets a permission that a GPC header, a GPP sale opt-out or a US Privacy
opt-out revoked, and a deployment that wants the opt-out to stand lists it
after `tcf`. A PMP answer read by `mtm` is asked after `tcf`, so it answers
over `tcf` and over every opt-out for the eleven named Data Uses (§4.6). Which
scheme wins is a question about a jurisdiction and a publisher, so the
deployment decides it. The `signals.tcf.authoritative` flag governs only the
answers the `tcf` module gives about permissions. With the flag false the
module answers `Neutral` for every permission and declares nothing grantable,
but a record refusing storage still withdraws (§4.2).

A module is also given the whole ordered list and its own place in it, and
may ask a named peer what that peer makes of a permission. That makes a rule
such as "this answer supersedes a refusal only when Global Privacy Control
made it" expressible inside the module that wants it. A consultation goes
one level deep, so two modules asking each other cannot loop.

Removing `gpc` from the list stops the GPC module running, but the consent
pipeline can still synthesize a US Privacy opt-out from the same header for a
US privacy state (§4.4), which the `us-privacy` module then acts on. A
deployment that wants the header to have no effect also turns that synthesis
off with `[consent.us_privacy_defaults] gpc_implies_optout = false`.

Two simplifications against the draft's taxonomy, both recorded in §11:

- **Of the shipped modules, TCF and `mtm` are the grant-class signals.** The
  draft's grant class also admitted explicit GPP/USP non-opt-out values,
  regime-scoped, so a US rule could be `requires_signal` yet grant on
  signal-carrying traffic. The implementation instead expresses the US posture
  as a `granted` baseline that opt-outs revoke, so no-signal US traffic is
  allowed rather than blocked pending a signal. Explicit non-opt-out GPP/USP
  values grant nothing on their own.
- **An unreadable record is answered per scheme, not per section.** The draft
  blocked grants per record family and per mapped section of a GPP string. The
  implementation scopes the answer to the Data Uses of the scheme whose record
  could not be read, and does not look inside the sections of a GPP string
  (§4.5), so an unreadable record from one scheme leaves the other schemes'
  answers alone.

### 4.1 Decision matrix

For each permission, with baseline _B_ from the resolved rule, under the
default order and a request with no `__mtm_pref` cookie, so that `mtm` is
silent:

| Signal state                                                                         | B = granted | B = requires_signal | B = denied |
| ------------------------------------------------------------------------------------ | ----------- | ------------------- | ---------- |
| TC string present but undecodable, Data Use mapped by a TCF purpose                  | unset       | unset               | unset      |
| TCF present, consents to the mapped purpose, with or without an opt-out              | set         | set                 | unset      |
| TCF present, refuses the mapped purpose                                              | unset       | unset               | unset      |
| Opt-out present or its string undecodable, Data Use in the revoke set, no TCF answer | unset       | unset               | unset      |
| No signal (or neutral for this Data Use)                                             | set         | unset               | unset      |

With an opt-out listed after `tcf`, a request carrying both resolves unset
under every baseline for the Data Uses that opt-out revokes. An expired TCF
record resolves as the "no signal" row. A `__mtm_pref` cookie of `standard` or
`personalized` adds a last answer for the eleven named Data Uses under the
default order, which replaces the result in every row above for those Data
Uses (§4.6). A permission whose baseline is `requires_signal` and that no
module has answered is also listed as `awaiting` on the page when some
configured module could still grant it, and a permission that a signal refused
is never listed as awaiting (§13). Whether an unset outcome is also a
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
   purpose-refusing string. The consent pipeline reads only the live request
   (its cookies, its headers and the resolved location), so the withdrawal
   decision sees live request signals only, satisfying the draft's
   live-request constraint by construction.
2. **US-style opt-outs never withdraw.** GPC and sale opt-outs are use
   restrictions, which suppress the permissions the policy revokes (EC
   headers stripped, nothing egressed) but never trigger destruction, so
   lifting the opt-out restores the identity.
3. **An unreadable record never withdraws.** The `tcf` module reads the record
   the consent pipeline decoded, so a TC string that could not be decoded
   withdraws nothing, whatever the module answers for the permissions it maps
   (§4). Destruction requires an affirmative, decodable signal.
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

A module answers withdrawal separately from its signal, through
`withdraws` on `PermissionSignalModule`, and core scopes any answer to the
baseline (`withdrawn` in `permission_signal/mod.rs`), carrying the result as
`PermissionState::storage_withdrawn`. Of the shipped modules only TCF answers
it, and its answer does not read `signals.tcf.authoritative`, so with the flag
false a record refusing storage still withdraws while the module's answers
about permissions are silenced (the test
`a_non_authoritative_policy_silences_the_record_but_not_the_withdrawal` in
`crates/permission-signal/tcf/src/lib.rs`). The response path acts on the
answer only where `ec_allowed` is false (`ec_finalize_response` in
`ec/finalize.rs`), so a request on which a later module such as `mtm` sets
storage after a TCF refusal withdraws nothing. Arms 1 to 4 above have direct
coverage in `crates/trusted-server-adapter-axum/tests/permission_signals.rs`,
with the real modules assembled. Refusal under `requires_signal` withdraws,
refusal under `granted` does not, consent does not, GPC alone does not, sale
opt-outs do not, and no signal does not. An unreadable record does not
(`ec/consent.rs` and the same Axum file), and a deployment whose list leaves
out `tcf` cannot withdraw at all. No test covers a withdrawal beside an `mtm`
grant of storage.

### 4.3 Withdrawal durability (largely deferred)

Implemented behavior (`ec/finalize.rs`): when the request carries the
withdrawal signal and the client presented a cookie, the response expires
the EC cookie, and the identity-graph tombstone is written for each
presented identifier the module accepts (the incoming cookie value and
the active value). The tombstone is the authoritative revocation marker for
subsequent EC behavior. A tombstone write failure is logged at warn level,
saying the row may still be live with consent granted, and the request
completes, so the write is best effort.

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
normalizer. A module reads its output as the decoded consent record on
`SignalInput`, and a module for a scheme the pipeline does not decode (`mtm`)
reads the request evidence instead. The pipeline reads the live request only,
being its cookies, its headers and the resolved location. The implemented
pipeline:

1. Extract raw signals from cookies and headers, and decode TCF v2, GPP,
   and US Privacy. A decode failure keeps the raw string and leaves the
   decoded field empty, which the module for that scheme reads as an
   unreadable record (§4).
2. Resolve standalone-TCF vs GPP-embedded-TCF conflicts per the configured
   mode (`restrictive`, `permissive`, `newest`), preserving the pre-epic
   selection algorithm.
3. Apply the expiry check, where a TCF record older than the configured maximum
   age has its decoded form cleared, the `expired` flag set, and its raw
   string left in place. Expiry is its own state, excluded from the
   unreadable case, and resolves as absent for acquisition.
4. Construct a US Privacy string from GPC for US privacy states with no
   explicit USP cookie, so the opt-out also travels in transport fields. The
   setting `[consent.us_privacy_defaults] gpc_implies_optout`, true by default,
   controls it.

**Raw strings after assembly.** Once the permission state is assembled,
`EcContext` keeps only the raw consent strings that a module vouched for
(`ConsentContext::keep_only`, called when `EcContext` is built in
`ec/mod.rs`). A module vouches for a string by reporting it through
`valid_signal` as one it read and used, and the `tcf`, `gpp` and `us-privacy`
modules do so only for a string the pipeline decoded. An unreadable string, an
expired one and one whose module is not listed are therefore removed, and the
auction paths, the publisher paths and the identity-graph row (§1) see only the
raw strings that were read and used. The same list reaches the page as
`signals` (§13).

The draft's declared reordering, expiry filtering **before** conflict
resolution, was **not implemented**: conflict resolution still runs first,
so the pre-epic order stands. Recorded in §11.

**Persisted-KV consent (not built).** The draft's fallback to the consent
persisted for an EC ID does not exist on this row. `ConsentPipelineInput`
takes no identity-graph input, `ConsentSource` has no persisted variant, and
`consent_store` is refused as an unknown key in `[consent]`. The plan that
removed the path is
`docs/superpowers/plans/2026-07-13-issue-883-remove-legacy-consent-store.md`
(IABTechLab/trusted-server#883). A request that carries no consent signals
therefore resolves from the place baseline and whatever the modules read from
the request. The identity graph keeps only the `consent.ok` flag and the raw
strings of the row (§1).

**Proxy mode.** Proxy mode still skips semantic decoding entirely. The
draft's minimal opt-out extraction was not implemented, and the fail-open
consequence the draft feared does not arise. A TC, GPP or US Privacy string in
proxy mode is present but undecoded, so the module for its scheme reads it as
unreadable (§4) and revokes the Data Uses that scheme covers. The GPC header
needs no decoding, so the GPC opt-out is honored directly. No grant is derived
from a TC, GPP or US Privacy string in proxy mode, and `mtm` still answers from
its cookie. Absent records resolve to the baseline. No module vouches for an
undecoded string, so `keep_only` removes the raw strings, and by reading the
code the OpenRTB consent fields on the auction and publisher paths carry none
in proxy mode. No test covers proxy mode through `EcContext`, and Open
questions, item 2, asks whether that is intended.

### 4.5 US signal field mapping

Implemented sources, as declared in the shipped `signals` section:

| Source                                             | Effect                                   |
| -------------------------------------------------- | ---------------------------------------- |
| `Sec-GPC` request header                           | US-style opt-out                         |
| GPP US section sale opt-out                        | US-style opt-out                         |
| US Privacy `opt_out_sale = Y`                      | US-style opt-out                         |
| GPP or US Privacy string that could not be decoded | US-style opt-out for its own source (§4) |
| US Privacy `opt_out_sale = N`                      | Nothing (no grant class exists for USP)  |
| Any explicit Not Applicable value                  | Nothing                                  |
| Absent / unknown / reserved values                 | Nothing                                  |

An opt-out revokes the Data Uses the policy's `revokes` value names. The
sample, `config/permissions/sample.yaml`, lists device storage, sale,
sharing, third-party sale, first- and third-party targeted advertising, and
profiling, so an opted-out visitor gets no Edge Cookie written and no
identifier shared while contextual advertising and measurement continue. That
is broader than the draft's mapping, which scoped sale opt-outs to
personalized-ad selection only, and a deployer changes it by editing the list
or writing `revokes: all`. Each source is read by its own module (`gpc`,
`gpp`, `us-privacy`), which runs only when
`[permission-signal] modules` names it or the key is omitted, and whether an
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

### 4.6 The Model Terms for Marketing module

The `mtm` crate (`crates/permission-signal/mtm`) reads the answer of the
Preference Management Platform (PMP). The PMP asks a visitor one question and
keeps one of three words, and its page script mirrors the word into the first
party cookie `__mtm_pref` (`COOKIE_MTM_PREF` in core) so that the server can
see it. A value that is not one of the three words is read as absent. The
module does not read the policy, so the `signals` section of `permissions.yaml`
cannot silence it, and it reads the cookie from the request evidence, so proxy
mode does not affect it.

| Word            | Answers `Grant` for                                | Answers `Revoke` for    |
| --------------- | -------------------------------------------------- | ----------------------- |
| `standard`      | Data Uses 1, 2, 7, 8, 9, 10 and 11 of the §2 table | Data Uses 3, 4, 5 and 6 |
| `personalized`  | All eleven                                         | None                    |
| `non-marketing` | None                                               | None                    |

The word `non-marketing` is a recorded answer that changes no permission, so
the baseline stands. Under the shipped `gdpr-eu` and `gdpr-uk` groups the
baseline grants no marketing Data Use, and under `us-opt-out` the baseline
grants them. The module also:

- declares the eleven Data Uses grantable (`grants`), so a page waits for them
  while the PMP question is open (§13),
- reports the word as a valid signal of scheme `mtm`,
- declares the versioned Model Terms for Marketing,
  `https://m4ow.uk/mtm/2.txt`, as the terms the data is available under
  whenever a word is present, `non-marketing` included, and
- never withdraws.

The cookie is written by script and is not `HttpOnly`, by design, because the
script that owns the question has to read and replace the cookie (the test
`the_preference_cookie_is_never_httponly` in the crate checks the PMP's own
`Set-Cookie` strings for `HttpOnly`). The server therefore cannot tell the
PMP's write from another script's. Under the default order `mtm` is asked
last, so a request carrying `standard` or `personalized` has `mtm` answer after
`tcf` and after every opt-out, and replace their results for the eleven named
Data Uses. A deployment that wants a TCF record or an opt-out to stand over a
PMP answer lists `mtm` before `tcf` and the opt-outs. The Axum test for `mtm`
(`a_pmp_answer_in_the_eu_settles_what_the_model_terms_cover_and_declares_them`)
gives `mtm` no TCF record or opt-out to answer over, so no test covers that
order (Open questions, item 5).

### 4.7 What each adapter runs

| Adapter    | Signal modules offered, in the default order | Geo when `[geo] module = "platform"`                                                                                                                                      |
| ---------- | -------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly     | `gpc`, `gpp`, `us-privacy`, `tcf`, `mtm`     | `crates/geo/fastly` wraps the SDK's `geo_lookup`, which returns an `Option` and turns every failure into `None`                                                           |
| Cloudflare | The same five                                | Reads `cf-ipcountry` on every plan and `cf-region-code` only with an Enterprise plan and the visitor-location managed transform, so without them no region rule can match |
| Axum       | The same five                                | Resolves nothing                                                                                                                                                          |
| Spin       | The same five                                | Resolves nothing                                                                                                                                                          |

With `[geo] module` unset or `none`, every adapter uses `DisabledGeo`, which
resolves nothing (§5.2). Each adapter holds its own copy of the module list in
the `shipped_signal_modules()` function of its `app.rs`. No test compares the
copies with one another or with the order the Axum tests assume, and only the
Axum adapter's tests (`tests/permission_signals.rs`) assemble the real modules.
The permissions inspector (`tools/permissions-inspector`) holds a list of its
own that leaves out `mtm`.

## 5. Jurisdiction resolution

### 5.1 Order

Geo resolution runs **before** permission resolution. Jurisdiction is an
input to the permission set, which is why geo modules cannot themselves
be gated on it (providers spec §5). The selected geo module resolves a
country and optional region, and the tree is walked most specific first, being
the region node, then the country node, then the top node, matched
case-insensitively. Implemented in
`EcContext::read_from_request_resolving_geo` and
`PermissionMaps::rules_for`.

### 5.2 Lookup failure

Implemented, with the failure state carried explicitly:
`GeoStatus { Located, NoLocation, Failed }` (in `ec/consent.rs`) separates
"the module resolved no location" from "the lookup errored". A **failed**
lookup resolves every permission to the **requires-signal floor** and its
jurisdiction to `unknown`, never the tree's top node. The failure is logged at error
level so it is visible. The storage-withdrawal baseline follows the same
floor, so a failed lookup cannot widen destructive withdrawal either. The
rule is proven through the seam by
`a_geo_module_failure_resolves_permissions_at_the_requires_signal_floor`
in `ec/mod.rs`, which drives a `PlatformGeo` that returns an error rather
than constructing the status by hand.

**Which modules can reach it.** The floor is the contract for a geo
module that performs its own fallible lookup, and none of the modules
shipped in this workspace is one. Fastly's `geo_lookup` returns an
`Option` and the SDK collapses every hostcall, buffer and parse failure
into `None` before it reaches the caller. The Cloudflare module reads
request headers, which cannot error. The Axum and Spin modules resolve
nothing at all. `DisabledGeo`, the default whenever
`[geo] module` is not `"platform"`, returns nothing by construction. So
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
unreadable, or refusing evidence sets nothing.

Not implemented: a lookup-failure metric (the error log is the signal
today) and the draft's `regime = "gdpr"` component of the failure profile,
since no regime concept exists. The draft's capability check, that an
adapter whose geo implementation can never resolve anything must not accept
the selection, is part of the providers-spec module qualification rather
than this model.

### 5.3 No geo module selected

Every request resolves at the top of the rules tree, so jurisdiction becomes
a static constant, which is only honest when the operator can genuinely
assert single-jurisdiction traffic.

Constraint, implemented in
`GeoConfig::validate_jurisdiction_acknowledgment`: **startup fails** when
an Edge Cookie module is configured and no geo module is selected,
unless the operator sets `[geo] assume_single_jurisdiction = true`. This
makes the dangerous migration config (a permissive top `group` with
geo unset) an explicit operator decision rather than an accident, closing
the highest-severity finding of the PR #838 review.

The guard's consumer list is narrower than the draft's. The draft enumerated
every jurisdiction consumer (EC module, regime-gated auction dispatch,
raw-EC and EID egress). In the implementation the EC module is the only
consumer whose behavior the policy gates, because auction dispatch was not
migrated (§7.4), so the guard fires on the EC module selection alone. When
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

- the geo module resolved no location (or none is configured), and
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

| Condition                                                    | Resolution behavior                                                                                                                                                                                                                                          |
| ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Geo lookup reports a failure                                 | Requires-signal floor for every permission and an `unknown` jurisdiction, never the top-node baseline, with the error logged. No module shipped today can report one, so a host geo outage resolves as no location, at the top-node baseline, instead (§5.2) |
| No geo module configured                                     | Top-node baseline, guarded by `assume_single_jurisdiction` (§5.3)                                                                                                                                                                                            |
| Country resolved, no matching node                           | Top-node baseline (§5.4)                                                                                                                                                                                                                                     |
| Region resolved, no region node                              | Country node's group, and the nearest ancestor's `jurisdiction`                                                                                                                                                                                              |
| Top node missing `group` or `jurisdiction`                   | Startup failure (§3.3)                                                                                                                                                                                                                                       |
| EC module configured, no geo, no acknowledgment              | Startup failure (§5.3)                                                                                                                                                                                                                                       |
| Module name the adapter does not link, or a name given twice | Startup failure when the adapter builds its application state, and not at settings validation (§3.3)                                                                                                                                                         |
| Malformed `permissions.yaml`                                 | Parse error at settings load, once per instance, because the embedded file is a build-time constant, never per-request                                                                                                                                       |
| Undecodable record present (TCF, GPP, or USP)                | Read by the module for that scheme as the refusal or opt-out it may have carried, on the Data Uses that scheme covers, and never a withdrawal. Core answers nothing ahead of the modules, so the order decides against the other schemes (§4)                |
| Expired TCF record                                           | Distinct state, not unreadable. The decoded record is cleared, the `tcf` module answers `Neutral`, and the baseline applies                                                                                                                                  |
| Signals contradict (opt-out plus consent)                    | The later module in the configured order decides, so under the default order a TCF consent amends an earlier opt-out and a PMP answer read by `mtm` amends both (§4)                                                                                         |
| No EC module selected                                        | Identity fails closed, with nothing created and an incoming cookie value never used or egressed (§7)                                                                                                                                                         |

The posture is fail-closed. Every ambiguous state resolves to the
configured baseline or more restrictive.

### 6.1 Upgrading from the earlier settings

A configuration written for the earlier model fails at startup on this row,
each time with a message that names what to write instead. The rollout design,
`2026-07-30-provider-migration-rollout-design.md` (§4 and §5), carries the full
migration for `[ec]`, `[device]` and `[geo]`. For the permission model:

1. Remove `[geo] default_country`. `[geo]` refuses any key other than `module`
   and `assume_single_jurisdiction`, and the top node of the `rules:` tree takes
   the place of the old key (§5.4).
2. Remove `[consent.gdpr]`, `[consent.us_states]` and `consent_store`.
   `[consent]` refuses a key it does not define, and the `rules:` tree holds the
   two lists (§3.4). The configuration guide, `docs/guide/configuration.md`,
   still lists the two lists and `consent_store`.
3. Set `[geo] assume_single_jurisdiction = true` where an Edge Cookie module
   runs and no geo module is selected (§5.3).
4. Write the table as `[permission-signal]`, with `modules` for the earlier
   `module` or `sources`. The earlier forms are refused with a message that
   names the new one.
5. Check the names in `modules` against the crates the adapter links. Only an
   adapter finds a wrong or repeated name, because settings validation and the
   `ts` command line tool link none of the module crates. When an adapter cannot
   build its state it serves its startup error router in place of the
   application.
6. Decide the order. The default order lets a TCF answer amend an opt-out,
   which is the opposite of the earlier fixed rule, and a deployment that wants
   the earlier outcome lists the opt-outs after `tcf` (§4).
7. Put the policy in the build. The build compiles in
   `config/permissions/sample.yaml`, a sample for testing and evaluation, and
   no other file or setting supplies policy on this row (§3.1). A deployment
   replaces that file and rebuilds.

## 7. Enforcement points

Consumers of the resolved set in the implementation:

1. **EC module execution.** The module declares
   `required_permissions()`, core resolves a `PermissionState` once per
   request at `EcContext` construction, and the module executes only when
   every declared permission is set (`ec_allowed`). A module that
   requires nothing always runs. **Geo** is ungated because gating it is
   circular, jurisdiction being an input to permission resolution.
   **Device** is ungated by a separate, deliberate decision, because its
   security-classification role must run for traffic that has granted
   nothing, and operator selection is the recorded authorization (modules
   spec §5) for the built-in and host modules, which declare nothing. No code
   on this row reads the `required_permissions()` of a device or geo module. A
   device module that an integration module supplies arrives with row 7 of the
   stack (`split/7-integration-seam-impl`, IABTechLab/trusted-server#1348),
   where `resolve_device_module` in
   `crates/trusted-server-core/src/integrations/registry.rs` refuses at startup
   a device module that declares a permission, naming the module and the Data
   Uses, because no per-request device gate exists to honor the declaration.
   The built-in Edge Cookie modules declare `necessary.operations.storage`.

2. **EC lifecycle.** Creation requires the module's declared permissions
   through the gate above. Withdrawal follows §4.2. Recognition and
   revocation of an existing identifier are never permission-gated, and the
   withdrawal path runs precisely when `ec_allowed` is false, reading the
   raw cookie value kept for that purpose.

3. **Sharing beyond the edge.** One predicate,
   `EcContext::ec_sharing_allowed`, requires the module gate plus
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

   With **no EC module configured**, identity fails closed, meaning the gate is
   closed rather than open by default (`ec_allowed` is false), so a cookie
   value present on the request is treated as absent and never used or
   egressed. This replaces PR #838's vacuously-true `is_none_or` check.

   **Navigation and page-bids.** Both attach the EC-derived request ID and
   `user.id` under the sharing pair (`ec_sharing_allowed`), as the table above
   lists (`handle_publisher_request` and `handle_page_bids` in `publisher.rs`),
   so a storage-only grant keeps first-party use and places no EC in `user.id`
   on those paths. The draft's fuller inventory (proxy/click/Testlight
   forwarding gates, the observability denylist with typed redaction
   boundaries, the raw-regulatory-transport destination allowlist, integration
   response cookies) is deferred (§11). Today EC values are truncated
   (`log_id`) before logging as the observability mitigation.

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

The client-cycle resolve endpoint (`POST /_ts/api/v1/ec/resolve`) is a
further consumer, and this row does not build it. Row 5 of the stack builds it
(`split/4-client-resolve`, IABTechLab/trusted-server#1346), where
`handle_ec_resolve` in `ec/resolve.rs` answers 204 and accepts no identifier
while `ec_allowed` is false. Its design is
`2026-07-30-client-cycle-ec-resolve-design.md`.

### 7.1 Contextual OpenRTB v1 allowlist (deferred)

Not implemented. The machine-readable projection manifest, its path
grammar, cardinalities, cross-field rules, and derivation vocabulary, and
the conformance walker over final encoded bytes, belong to the dispatch
migration (§7.4) and remain specified by the 2026-07-31 draft for that
work.

## 8. Testing strategy

Implemented, in `permissions.rs`, `permission_signal/mod.rs`,
`ec/consent.rs`, `ec/mod.rs`, `ec/finalize.rs`, `consent/mod.rs`, the five
module crates, the page script, and
`crates/trusted-server-adapter-axum/tests/permission_signals.rs`:

- **Signal order.** The last module with an opinion decides, silence
  leaves an earlier answer standing, and silence from every module is not
  a refusal. One test per opt-out source shows a TCF answer applied over it
  under the default order (`a_prompt_answer_applies_over_a_gpc_signal` and
  companions), and `the_opt_out_wins_when_a_deployment_puts_it_last` shows
  the reverse. An omitted `modules` runs every module in the offered order,
  a configured list is the order they run in, an empty list is accepted, and
  an unknown or repeated name is refused. The tests in
  `permission_signal/mod.rs` use stand-in modules named `gpc` and `tcf`,
  whereas the real crates answer to `permission-signal.gpc` and
  `permission-signal.tcf`, and the Axum test
  `the_documented_names_select_every_shipped_module_in_order` is the one that
  selects the real crates by name.
- **Withdrawal scoping.** One test per §4.2 arm: refusal under
  `requires_signal` withdraws, refusal under `granted` suppresses without
  destroying, consent is not a withdrawal, GPC alone never withdraws, sale
  opt-outs never withdraw, no signal never withdraws, an unreadable record
  never withdraws.
- **Unreadable and expired records.** Each of the `tcf`, `gpp` and
  `us-privacy` crates tests that an unreadable string of its own scheme is read
  as that module's refusal or opt-out, and core is tested to answer nothing
  ahead of the modules
  (`core_answers_nothing_ahead_of_the_modules`). The Axum tests show the order
  deciding between a readable and an unreadable record, and an expired TC
  string is not treated as unreadable
  (`an_expired_tcf_record_is_not_treated_as_malformed`).
- **Geo status.** A failed lookup resolves at the requires-signal floor
  (permissions and the storage baseline both), driven through a
  `PlatformGeo` that returns an error rather than from a hand-built status
  (`a_geo_module_failure_resolves_permissions_at_the_requires_signal_floor`),
  and no location falls back to the top node of the `rules:` tree.
- **Policy parsing and validation.** Groups, rules, detailed-rule
  acquisition maps (including `requires_signal` per rule), and the
  signals-section round trip. One table of fifteen cases
  (`from_yaml_rejects_each_malformed_policy`) checks the refusals, being
  unknown groups, unknown permissions, unknown acquisitions, incomplete
  groups, case-insensitive duplicate rule keys, unknown revoke keywords, the
  jurisdiction rules and places written under a region. No test covers a
  misspelled key in a node, or `us-state` under a country other than `US`.
- **Shipped-table coverage.** All 30 EU/EEA codes lock storage to
  requires-signal (§3.5).
- **Vocabulary breadth.** A TCF record grants or revokes every one of the
  eleven mapped purposes, not only storage and personalized ads.
- **Gates.** Module execution blocked without its required permissions,
  the sharing pair withheld on a storage-only grant, EIDs stripped when
  either permission of the pair is unset, and the no-module stateless
  posture.
- **The `mtm` crate.** Fourteen tests in the crate cover the three words, the
  Data Uses each answers for, the terms it declares, and that no word
  withdraws. The PMP's own `Set-Cookie` strings are parsed to the word they
  carry, and the cookie is pinned as never `HttpOnly`. One Axum test runs all
  five modules for a PMP answer in the EU.
- **Page script.** `crates/trusted-server-js/lib/test/core/permissions.test.ts`
  covers how the snapshot, with its signals and terms, reaches page code in
  every order of arrival.
- **Not covered by a test.** The order between `mtm` and `tcf` or an opt-out in
  one request. A withdrawal beside an `mtm` grant of storage. The startup
  warning with the real module names. Proxy mode through `EcContext` and
  `keep_only`. A misspelled key in a policy node, and `us-state` under a
  country other than `US`. The module lists of the four adapters, which no test
  compares. The permissions inspector, whose CI job
  (`.github/workflows/inspector.yml`) builds the engine and runs nothing
  against it.

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

**Open questions.** Each of these waits for a decision and for a code change,
and this revision changes no code.

1. Whether the startup warning that lists the modules a list leaves out should
   compare names the way selection does. `omitted` in
   `permission_signal/mod.rs` compares each written name with the full module
   name, and every shipped module is named `permission-signal.<folder>`. By
   reading, a list written as `["gpc", "tcf"]` therefore reports all five
   modules as not acted on, those listed included. The tests use stand-in
   names equal to the written ones, so they cannot show it.
2. Whether `proxy` mode should keep passing the raw consent strings on. By
   reading, it passes none, because the pipeline decodes nothing in that mode,
   no module vouches for an undecoded string, and `keep_only` removes the raw
   strings (§4.4). `docs/guide/configuration.md` and the comments on
   `ConsentContext::expired` and `build_consent_context` still say that proxy
   mode forwards them.
3. Whether the policy parser should refuse a key it does not define (§3.1).
   A key beside `group`, `jurisdiction` and `permissions` is read as a place
   code, a key outside `groups`, `rules` and `signals` is ignored (the sample
   file carries `name:`), and so is an unknown key inside `signals`,
   `signals.tcf` or `signals.us_opt_out`.
4. Whether `us-state` should be refused on a region under a country other than
   `US` (§3.2).
5. Whether the default order should keep `mtm` last. A script can write
   `__mtm_pref`, and under the default order a `standard` or `personalized`
   word replaces a TCF refusal and every opt-out for the eleven named Data
   Uses, and stops a TCF refusal of storage from withdrawing (§4.2 and §4.6).
6. Whether to correct, with the code, the text that now says otherwise.
   - `trusted-server.example.toml` lists four modules and says that an
     unreadable record revokes whichever modules are configured.
   - The comments in `config/permissions/sample.yaml` name
     `[permission_signal] module`.
   - `docs/guide/configuration.md` lists `gdpr.applies_in`,
     `us_states.privacy_states` and `consent_store`.
   - The comment on `request_jurisdiction` in `consent/mod.rs` mentions a KV
     read fallback that no longer exists.
   - The header of `permission_signals.rs` in the Axum tests counts four
     modules, and the comment on `shipped_signal_modules()` in each adapter
     counts three after the first.

## 10. Divergences from issue #779

This spec supersedes #779 on the following points, so there is one
acceptance contract, not two:

| #779 says                                                                                    | This spec says                                                                                                                                                                                                                     | Why                                                                                                                                                                        |
| -------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Unmatched countries fall to `default_country`                                                | Adopted, with a changed mechanism since 2026-09-01. Unmatched and unresolved requests both fall to the required top node of the `rules:` tree, and a **failed** lookup floors instead (§5, §12)                                    | The failure state is the one that must never reach a permissive default, and the draft's `rules.default` split was not kept                                                |
| The full TCF purpose vocabulary is modeled                                                   | Adopted and extended. All eleven purposes are signal-resolved, and the full Privacy Taxonomy is carried as declared baseline (§2)                                                                                                  | The joint taxonomy work made whole-taxonomy declaration the goal, and `denied` defaults keep undeclared uses inert                                                         |
| Policy is an embedded file                                                                   | Adopted. `permissions.yaml` is compiled into the build (§3.1), and runtime configuration is deferred follow-up                                                                                                                     | The runtime push and activation pipeline does not exist, and version control is the audit trail meanwhile                                                                  |
| Permission sources are open-ended (#777: publisher interaction, external services may grant) | Adopted as a seam. A source is a permission signal module, a crate implementing `PermissionSignalModule`, listed by each adapter in `shipped_signal_modules()` and asked in the order `[permission-signal] modules` gives (§1, §4) | The five shipped schemes already use it from outside core, so the interface has real consumers, and publisher interaction or an external service arrives as another module |

## 11. Revision record vs the 2026-07-31 draft

One row per divergence between the draft and the implementation this
revision was verified against (branch `split/3-permissions` at `a028c00da`,
row 4 of the stack).

| Draft position                                                                                                                                            | Implemented position                                                                                                                                                                                                                                                                  | Why                                                                                                                                                                                           |
| --------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Vocabulary is two TCF-purpose identifiers, enforced permissions only (§2)                                                                                 | IAB Privacy Taxonomy Data Uses, with eleven named purposes all signal-resolved, plus 53 further taxonomy Data Uses that no module gates on and that a signal reaches for eleven of them under the sample policy (§2)                                                                  | The joint taxonomy adoption postdates the draft, and whole-taxonomy declaration serves completeness and demonstration, with `denied` defaults keeping unenforced flags inert                  |
| Policy lives in `[permissions]` in `trusted-server.toml`, published via `ts config push` (§3.1)                                                           | Policy is `permissions.yaml`, compiled into the build with `include_str!`, parsed once and covered by tests                                                                                                                                                                           | The runtime config push and activation apparatus does not exist, so publishing runtime policy without it would recreate the hazards the draft cataloged. Runtime policy is deferred follow-up |
| Every group carries a required `regime` class read by auction dispatch (§3.2)                                                                             | No regime field exists                                                                                                                                                                                                                                                                | Its only consumer, regime-gated dispatch, was not migrated, and the field returns with that work                                                                                              |
| Overrides name explicit acquisition rules, replacing the `+`/`-` sigils (§3.2)                                                                            | Adopted. A detailed rule's `permissions` map assigns `granted`, `requires_signal`, or `denied` per Data Use                                                                                                                                                                           | The draft's requirement, expressed in the YAML schema. `requires_signal` is now expressible per rule                                                                                          |
| Two fallbacks: policy `rules.default` for unmatched countries, `default_country` only for the static no-geo mode (§5.4)                                   | One required fallback covers unmatched and unresolved requests in every mode, startup-validated, and a failed lookup floors separately. It was `[geo] default_country` until 2026-09-01 and is now the top node of the `rules:` tree (§12)                                            | One deployer knob is simpler, and the safety-critical separation kept is failure vs absence, carried by `GeoStatus`                                                                           |
| Validation checks assigned ISO 3166-1 codes, assigned subdivisions, and a group identifier grammar (§3.3)                                                 | Validation covers unknown groups, permissions, acquisitions, revoke rules, incomplete groups, case-duplicate rule keys, and the jurisdiction values. The parser does not reject an unknown key in a node (§3.1)                                                                       | Smaller surface shipped first, and the EU/EEA coverage test guards the shipped table against the typo class. ISO-assignment checks are future hardening                                       |
| Three-class signal taxonomy with regime-scoped grant acceptance, where the US posture is `requires_signal` with GPP/USP non-opt-out values as grants (§4) | Of the shipped modules TCF and `mtm` are the grant sources, the US posture is a `granted` baseline that opt-outs revoke, and explicit non-opt-out values grant nothing                                                                                                                | A simpler two-signal model without regimes. The cost, no-signal US traffic is allowed by baseline rather than blocked pending a signal, is a deliberate policy choice in the shipped file     |
| Malformed-present blocks grants per record family and mapped section (§4.4, §4.5)                                                                         | Each module answers for its own scheme's unreadable record, as a refusal or opt-out on the Data Uses that scheme covers, and core answers nothing ahead of the modules (§4)                                                                                                           | The scope is the Data Uses of the scheme whose record could not be read, because per-section scoping needs the full §4.5 decoder work                                                         |
| Normalization runs expiry before conflict resolution, a declared change (§4.4)                                                                            | Conflict resolution still runs before the expiry check                                                                                                                                                                                                                                | The reordering was not implemented, though the expired state itself (distinct from unreadable, absent for acquisition) was adopted                                                            |
| Persisted-KV consent flows through the full normalization pipeline with an explicit TTL comparison (§4.4)                                                 | Not built. No persisted-KV consent exists on this row, the consent pipeline reads the live request only, and `consent_store` is refused (§4.4)                                                                                                                                        | The legacy consent store was removed (IABTechLab/trusted-server#883), so live consent is interpreted on each request                                                                          |
| Proxy mode gains minimal opt-out extraction (§4.4)                                                                                                        | Proxy mode still skips decoding, so a present TC, GPP or US Privacy string is unreadable to its module and revokes the Data Uses that scheme covers, and the GPC header opt-out is honored without decoding. By reading, no raw string reaches the auction and publisher paths (§4.4) | The permission-layer outcome is equally or more restrictive with no new decode paths, and this is revisited with the §4.5 decoder work                                                        |
| Withdrawal has four triggers including an explicit storage-withdrawal or authenticated deletion request (§4.2)                                            | The TCF Purpose 1 refusal under a non-granted baseline is the only trigger, and opt-outs, unreadable records, absence, and policy changes never withdraw (adopted)                                                                                                                    | No deletion endpoint exists to carry the extra trigger, so the narrowest destructive surface shipped first                                                                                    |
| §4.3 durability protocol: family records first, suppression and authority-state records, outbox, breaker, strong reads                                    | Cookie expiry plus best-effort identity-graph tombstones per presented identifier, with failures logged                                                                                                                                                                               | The protocol requires storage primitives (linearizable CAS, independent durability domains) the adapters do not yet qualify, so it is deferred with the providers-spec storage work           |
| §4.5 field mapping and §4.5.1 vendored registry snapshot (sharing/targeted opt-outs, embedded GPC, applicability, derived `gpp_sid`)                      | Opt-out sources are the GPC header, a GPP sale opt-out, and a USP sale opt-out. The revoke set is policy-declared, and the shipped file lists device storage, sale, sharing, third-party sale, targeted advertising and profiling (§4.5)                                              | The full decoder and registry vendoring are their own project, and the policy-declared revoke set gives deployers the scoping lever meanwhile                                                 |
| §5.5 activation: JCS policy digests, ordinals, activation register, journal, drains, admission leases                                                     | None of it exists, and the built binary is the policy identity                                                                                                                                                                                                                        | With no runtime policy there is nothing to activate, and the draft remains the reference design for the runtime-config follow-up                                                              |
| §3.4 single jurisdiction truth, and §7 dispatch gated on the policy regime with a contextual projection                                                   | Half adopted. Since 2026-09-01 the `rules:` tree carries regime applicability and the two `[consent]` lists retire into it (§3.4), while auction dispatch still keeps the consent-subsystem gate (effective TCF Purpose 1 for GDPR or unknown jurisdictions), with no contextual view | Dispatch migration is follow-up. The legacy-list drift risk the draft named still stands and is recorded rather than resolved                                                                 |
| Every raw-EC egress path is pair-gated, with per-row tests and a denylist check (§7)                                                                      | Pair gating is centralized in `ec_sharing_allowed` (auction endpoint `user.id`, publisher navigation and page-bids `user.id`, identify, pull sync) and `gate_eids_by_permissions` (EIDs everywhere). Batch sync checks row state only                                                 | Partial adoption. Aligning the remaining paths, the S2S stored-provenance authority, and the inventory tests is recorded follow-up                                                            |
| Identity rows never store raw consent strings, only normalized provenance and a digest (§1)                                                               | The identity-graph entry stores the raw TCF and GPP strings that a module vouched for with the row                                                                                                                                                                                    | The normalized provenance schema belongs to the providers-spec storage work, and until then rows carry the raw strings                                                                        |
| No signals block in policy, and the signal mapping is fixed in the spec                                                                                   | New. A `signals` section in `permissions.yaml` declares the opt-out sources and revoke set, with `tcf.authoritative` governing only TCF's own effect. The TCF purpose map has since moved into the TCF module crate (§13)                                                             | Moves signal policy from code into deployer-editable data, while what a scheme's own signal means stays with that scheme's module                                                             |
| The §5.3 no-geo guard covers every jurisdiction consumer                                                                                                  | The guard fires when an Edge Cookie module is configured with no geo module                                                                                                                                                                                                           | The EC module is the only policy-gated consumer today, and the trigger list grows when dispatch and further egress paths join the model                                                       |
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
| The US-state value carries the state code, as `us-state/<CODE>`                                                  | The value is bare `us-state`, valid only on a region node, and rejected at the top and on a country node. The parser does not check the country above the region (§3.2)                                 | The matched node already names the state, so repeating the code in the value invites the two disagreeing                                                                                        |
| `consent.gdpr.applies_in` and `consent.us_states.privacy_states` in the operator TOML carry regime applicability | Both retire into the tree, where the shipped file holds the same 31 GDPR countries inheriting `gdpr` and the same 20 privacy-law states as region children of `US` with `jurisdiction: us-state` (§3.4) | One document carries the baselines and the applicability together, so a policy owner keeps one list rather than two in step, and the drift risk §3.4 recorded has nothing left to drift against |

This revision leaves unchanged the group vocabulary and flags, the signal
precedence of §4, and the requires-signal floor for a failed geo lookup, which
stays distinct from having no location. Trusted Server still encodes no
jurisdiction's law, as the deployer states the policy and the software carries
it out.

## 13. Revision record: permission signal modules (2026-09-14)

Signal precedence moves from a fixed rule in code to the order a deployment
configures, and the shipped schemes move out of core into module crates. One
row per change. The rows describe the code on `split/3-permissions`, so they
include the changes that followed the first revision of this section on
2026-09-14, being an unreadable record answered by its own module, `awaiting`,
`signals` and `mtm`.

| Before                                                                                                                 | After                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | Why                                                                                                                                                                                                                                                                                         |
| ---------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Precedence fixed in code, most restrictive first, so an opt-out always beat a consenting TCF record                    | The order in `[permission-signal] modules`, where the last module with an opinion decides, validated and logged at startup (§4)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | Which scheme wins is a question about a jurisdiction and a publisher, and a fixed rule answers it in code for every deployment                                                                                                                                                              |
| A GPC header suppressed a Data Use that a TCF record consented to                                                      | The default order is `gpc`, `gpp`, `us-privacy`, `tcf` and `mtm`, so a TCF answer amends an earlier opt-out, and listing the opt-out after `tcf` restores the previous outcome                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | Global Privacy Control is a browser setting with no interface of its own, and the schemes after it carry an answer a person gave through an interface, so by default that answer amends the header the visitor arrived with                                                                 |
| Signals resolved inside core through a per-permission `ConsentSignal` closure, with further sources deferred (§1, §10) | A `PermissionSignalModule` trait in core, with GPC, the GPP sale opt-out, US Privacy, TCF and MTM each a crate under `crates/permission-signal/`, linked and listed by each adapter in `shipped_signal_modules()` and selected from `[permission-signal] modules`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          | A scheme core has never heard of plugs in without a change to core, and none of the five is privileged by being built in                                                                                                                                                                    |
| The TCF purpose to Data Use map in the `signals` section of `permissions.yaml` (§3.2)                                  | The map in the TCF module crate, `crates/permission-signal/tcf/src/mapping.rs`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | A deployment that runs no TCF carries no table of another scheme's numbers                                                                                                                                                                                                                  |
| Malformed-present as step 3 of the fixed order                                                                         | Each module answers for its own scheme's unreadable record, in the configured order, and core answers nothing ahead of the modules (§4)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Core no longer reads any scheme, so what an unreadable, absent or expired signal means is each module's decision, and the order decides between schemes                                                                                                                                     |
| Withdrawal decided in core from the TCF record                                                                         | A `withdraws` answer on the module trait, scoped by core to the storage baseline, and given today only by TCF (§4.2)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | Revoking and withdrawing stay different questions, and a new scheme can carry its own withdrawal                                                                                                                                                                                            |
| The permission state carries the resolved set only                                                                     | It also carries `awaiting`, the permissions that need a signal no module has given and that some configured module could still grant (each module declares what it can grant through `grants` on the trait), `signals`, one entry for each signal a module read and found valid, as received (each entry carries the module, the scheme and the raw string, so the page receives the raw strings the modules used), and `tdls`, the terms documents the configured modules declare, in module order with duplicates removed. The page receives all four, and an empty state is `{"awaiting":[],"set":[],"signals":[],"tdls":[]}`. Each `tdls` entry must address a document never edited once published, which the `Tdl` type documents and cannot enforce | A page has to tell a prompt that has not run from a visitor who refused, a bid request has to carry only signals a module could read, and a recipient needs the exact terms the data is available under. An empty `tdls` says no terms were declared rather than that anything is permitted |
| Opt-out-beats-TCF pinning tests                                                                                        | Order tests in both directions, per-source tests, and selection tests (§8)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 | The tests pin the configured behavior rather than the retired rule                                                                                                                                                                                                                          |
| No scheme read the PMP's answer                                                                                        | `mtm` reads the PMP's `__mtm_pref` cookie, answers for the eleven named Data Uses, and declares the versioned Model Terms for Marketing as the terms the data is available under (§4.6)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | The PMP keeps its own copy of the answer and mirrors it into the cookie, which is what lets the server see the answer at all                                                                                                                                                                |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/3-permissions` at `a028c00da`. Issues found, nine of them blocking. The nine were the claim that an unreadable record revokes everything ahead of the modules, four shipped modules where five ship, registration through an integration builder, a page state described with two members where it has four, a follow-up note that gave `ec_allowed` for two paths that use `ec_sharing_allowed`, the persisted consent, the resolve endpoint and the device module check that this row does not build, unknown rule fields that the parser does not reject, the Data Uses said to have no signal, and raw strings said to be kept for forwarding. This revision states what the code does in each case, adds the fifth module `mtm`, a table of what each adapter runs, an upgrade list and the privacy effects the round found, names the row that builds each item this row lacks, and leaves the code changes the round asks for under Open questions.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-08-25 | Revised against the implementation, with a row for each divergence from the 2026-07-31 draft (§11).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| 2026-09-01 | The `rules:` tree replaced the flat rule keys and retired `[geo] default_country` (§12).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 2026-09-14 | Permission signal modules replaced the fixed signal precedence with a configured order (§13).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| 2026-10-06 | Module became the word for what an operator selects.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| 2026-10-07 | A module is named by its crate folder, so the selector reads `[permission-signal] modules`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 2026-10-15 | Fixes from review round 1. An unreadable record answered by its own module with nothing ahead of the modules, the fifth module `mtm` with its own section, registration through each adapter, the four members of the page state, the sharing pair on navigation and page-bids, the persisted consent, resolve endpoint and device module check named as built by other rows or not at all, what the policy parser does not reject, the Data Uses a signal reaches, the raw strings removed after assembly, a table of what each adapter runs, the upgrade list, the open questions and the review log. |
