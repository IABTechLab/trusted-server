# Design Spec: Provider and Permission Model, Migration and Rollout

**Status:** Proposed. The implementation is carried by the series PRs #1043 to
#1047, none of which is yet merged to main, and this document was revised
against that series on 2026-08-25. The §8 sign-off rows remain the series'
decision ledger, and rows the implementation now satisfies are marked with
their PR so the task force can ratify rather than re-litigate.
**Author:** Engineering
**Issue references:** #777-#781 (epic)
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-permission-model-design.md`,
`2026-07-30-integration-response-header-hook-design.md` (not implemented in
the series, see its status)
**Last updated:** 2026-08-25

> **Context.** The provider/permission epic is a breaking change to a live
> identity system. PR #838's review showed that the riskiest part of such a
> change is not the new code but the transition, meaning silent
> misconfiguration modes, undeclared behavior changes discovered by deleted
> tests, and no written statement of which pre-change behaviors were
> guaranteed to survive. This spec is that statement. The epic has now been
> implemented as five stacked PRs (#1043-#1047, the "series" below), and this
> revision reconciles the spec against that series, keeping §2's matrix and
> §8's ledger as the record the task force ratifies. Any further
> implementation PR must reconcile its diff against §2's matrix and list
> every deliberate divergence in its description.

## The implemented series

Five stacked PRs, verified against the tree at
`split/5-response-hook-docs` (the head of PR #1047; the branch is rebuilt on
each rebase, so the PR is the stable reference). A sixth PR, #1084, adds the
integration provider seam spec on top of the series and changes no code:

1. **PR #1043, the Edge Cookie provider seam.** The `EdgeCookieProvider`
   trait (`id`, `generate`, `accepts_id`, `normalize_id_for_kv`,
   `required_permissions`, `resolve_from_client` in
   `crates/trusted-server-core/src/ec/provider.rs`) routes identifier
   creation, cookie read-back, and identity-graph keying through the
   selected provider. Global identifier bounds are enforced by core at every
   entry point (`MAX_EC_ID_LEN = 256` bytes and the cookie-safe alphabet
   `[A-Za-z0-9._~-]` in `ec/cookies.rs`), rejecting loudly and never
   rewriting, so the cookie value and the graph key cannot silently
   diverge. `[ec] provider = "none"` spells explicit statelessness. A
   configured `[ec.providers.*]` block with no selector, an unreferenced
   block, and a selector with no block are each startup errors. The
   deprecated `[ec] passphrase` form still starts for one release cycle,
   mapping to `provider = "hmac"` with a deprecation warning, and a
   configuration carrying both forms is rejected.
2. **PR #1044, device and geo selection.** `[device] provider` selects
   `builtin` (User-Agent only, the default) or the opt-in `fastly`
   classifier (`crates/device/fastly`, TLS JA4 and HTTP/2 signals).
   `[geo] provider` defaults to no geolocation, with `"platform"` opting in
   to the host lookup (`crates/geo/fastly`) and `"none"` spelling the
   opt-out. All four adapters route geo through the one
   `build_geo_provider` selector. The provider configuration structs carry
   `deny_unknown_fields`, so a mistyped key fails startup. The host-signal
   Edge Cookie provider (`[ec.providers.host-signals]`) ships opt-in.
   Whether the host-signal surface stays is an **open review question**
   (sign-off row 22), not a settled decision.
3. **PR #1045, the permission model.** Permission names follow the IAB
   Privacy Taxonomy Data Uses (`permissions.yaml`, compiled into the
   build). Signal precedence is fixed in code, most restrictive first,
   meaning an opt-out (Sec-GPC, GPP sale opt-out, US Privacy) suppresses
   the Data Uses the policy revokes even against a consenting TCF record,
   a present but undecodable record blocks baseline grants (fail-closed),
   and only then does a TCF record decide its mapped Data Uses.
   Destructive withdrawal is narrow, where only a TCF record refusing
   storage in a jurisdiction whose baseline did not grant storage expires
   the cookie and writes the identity-graph tombstone, and opt-outs never
   destroy an issued identifier. Sharing beyond the edge (bidstream
   `user.id`, the identify response, partner pull sync) requires storage
   plus personalised-ad selection, the same pair that gates bidstream
   EIDs. `[geo] default_country` becomes required, a failed geo lookup
   resolves at the requires-signal floor instead of the default, and a
   no-geo deployment running an Edge Cookie provider must set
   `[geo] assume_single_jurisdiction = true`.
4. **PR #1046, the hardened client-cycle resolve endpoint.**
   `POST /_ts/api/v1/ec/resolve` enforces publisher-origin `Origin` (403),
   a `text/plain` or `application/json` content-type allowlist (415), a
   body size cap (413), the global identifier bounds on the created
   identifier (400), and refusal to replace a different identity already on
   the request (409). The identity-graph row is written before the cookie
   is set, with no graph meaning no identifier is created and a graph
   write failure returning 503. Every response carries
   `Cache-Control: no-store`. A non-HttpOnly `ts-ecr` marker cookie tells
   the page script a resolve succeeded, fixing the re-post loop the
   HttpOnly Edge Cookie would otherwise cause, and a Rust test pins the
   marker name and the demo's fixed word against the page script source.
   The client-fixed demonstration provider compiles only under the
   `client-fixed-demo` cargo feature and production builds reject
   selecting the demo at startup.
5. **PR #1047, the documentation set.** Configuration reference for
   `[ec]`, `[device]`, and `[geo]`, the Edge Cookie guide rewritten around
   providers and the permission model, and the example configuration
   documenting every selector. The integration response-header hook was
   **stripped from the series** because the hook had no consumer, which is
   that spec's own rule for speculative surface. The hook spec is retained
   as the design bar for when its first consumer arrives.

---

## 1. Scope

Covers the transition of existing deployments from the hard-wired EC /
device / geo behavior to the provider architecture and permission model.
Applies to every implementation PR in the epic (now the implemented series
PRs #1043-#1047 and any follow-up), and to the operator-facing migration
guide that ships with the last of them (still outstanding, §7).

## 2. Behavior-preservation matrix

For each decision the system makes today, the target behavior after the
epic, and whether that is a preservation or a declared change. **Silent
changes are defects.** PR #838 changed six of these without declaring any.
The Status column now also records where the implemented series stands,
naming the PR. Rows the series did not touch keep their design-target text
for the follow-up work that will implement them.

| #   | Decision (today)                                                                                               | After epic                                                                                                                                                                                                                              | Status                                                                                                                                                                                                                           |
| --- | -------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | EU/GDPR request, no TCF consent → no EC                                                                        | Same (opt-in baseline)                                                                                                                                                                                                                  | Preserved. Implemented (#1045), where `gdpr-eu` requires a signal for storage, with an EU-27 plus EEA coverage test                                                                                                              |
| 2   | US-state request, sale opt-out → no EC, existing EC expired and tombstoned even beside a consenting TCF string | Opt-out suppresses use but does not delete the identity. Lifting the opt-out restores the identity. The positive contextual-auction projection (permission §7's `ContextualAuctionView`) remains the design for what may still dispatch | **Changed deliberately.** The separation is implemented (#1045), with opt-outs stripping egress and never tombstoning. The contextual projection is not implemented (sign-off 4 open), and today EIDs and `user.id` are withheld |
| 3   | US-state request, no signals at all → no EC (fail-closed)                                                      | The draft's recipe kept this via a `requires_signal` US rule with an extended grant-signal class                                                                                                                                        | **Changed by the shipped default (#1045), flagged.** `permissions.yaml` ships US as a `granted` baseline whose uses drop on any opt-out, so a no-signal US request receives an EC. A deployer edits the yaml to differ           |
| 3a  | US-state request, explicit not-opted-out GPP/USP value → EC allowed                                            | No US signal grants anything. N/A, absent, reserved, and unknown values grant nothing                                                                                                                                                   | Implemented (#1045) in a stricter form than drafted, where signals only revoke, so the drafted "explicit not-opted-out may grant" class does not exist (sign-offs 3, 17)                                                         |
| 3b  | US-state request, TCF record refusing Purpose 1, no US opt-out → no EC                                         | Refusal beats coexisting non-TCF grant signals                                                                                                                                                                                          | Implemented (#1045), where an authoritative TCF refusal revokes its mapped uses. Under the shipped `granted` US baseline the refusal suppresses without tombstoning                                                              |
| 3c  | Consent-record conflict modes, expiry, KV fallback, proxy mode                                                 | Per the permission spec §4.4 matrix, whose changed row is that malformed-present blocks acquisition                                                                                                                                     | Malformed-present fail-closed is implemented (#1045). The rest of the consent normalization pipeline is carried forward unchanged by the series                                                                                  |
| 3d  | Valid plus expired consent records, where conflict resolution can select the expired record                    | Expired sources drop before conflict resolution                                                                                                                                                                                         | Open. Not addressed by the series                                                                                                                                                                                                |
| 3e  | Only the GPP sale field (and USP) is consulted, with sharing/targeted opt-outs ignored                         | Sale, sharing, and targeted-advertising opt-outs deny the personalised-ads uses, and none affects storage or destroys identity                                                                                                          | Partially implemented (#1045), where sale, USP, and Sec-GPC are honored and never destructive. The sharing and targeted-advertising GPP fields are not yet decoded (sign-off 3 open)                                             |
| 3f  | Non-privacy-state US traffic (for example Wyoming) is non-regulated → EC allowed                               | Country-level `US` is a protective floor, region rules may be stricter, and regionless traffic never degrades to non-regulated                                                                                                          | Implemented (#1045), where `permissions.yaml` maps country `US` to `us-opt-out`, with state rows able to override                                                                                                                |
| 3g  | Graph rows persist JA4 class, H2 signal hash, and buyer-facing quality metadata                                | The draft discontinued these for new rows                                                                                                                                                                                               | **Contradicted by the series, flagged.** Rows still persist device signals, including signal hash prefixes, when the opt-in providers run (#1044/#1046). Tied to the open host-signal question (sign-off 22)                     |
| 4   | UK request, no TCF record → no EC                                                                              | Same, unless the policy deliberately adopts a `granted` storage baseline for GB with citation and sign-off                                                                                                                              | **The shipped default adopts `granted` storage for GB (#1045), flagged.** The citation and sign-off the draft required do not exist yet. The task force owns this row                                                            |
| 5   | No country resolvable (geo failure) → no EC (fail-closed)                                                      | Protective failure profile, where permissions resolve at the requires-signal floor and `default_country` is reserved for unmatched requests in acknowledged static-jurisdiction mode                                                    | Implemented (#1045), where a failed lookup resolves at the requires-signal floor, logged at error level, and never falls back to `default_country` (sign-off 18)                                                                 |
| 6   | Non-regulated country, TCF record refusing Purpose 1 → EC still created, identity never tombstoned             | Refusal blocks new grants everywhere, and existing identity is never tombstoned where the baseline is `granted`                                                                                                                         | Implemented (#1045), where an authoritative refusal revokes its mapped uses everywhere and withdrawal is scoped to non-granted baselines                                                                                         |
| 7   | Country resolved but in no regulation list → EC created, EIDs pass through                                     | Governed by the deployment's default rule. The implementation expresses this as the required `[geo] default_country`, naming the `permissions.yaml` rule for unmatched requests                                                         | Implemented (#1045) with a changed mechanism, since no `rules.default` entry exists and `default_country` is required and validated at startup                                                                                   |
| 8   | Opt-out signal outside US states → ignored today                                                               | Mapped use restrictions are honored globally, and opt-outs never tombstone identity                                                                                                                                                     | Implemented (#1045), where the signal mapping is jurisdiction-free and suppresses even TCF-consented uses, without destruction (sign-off 1)                                                                                      |
| 9   | Fastly bot gate requires JA4 plus platform class before KV-backed EC writes                                    | The draft deferred host signal processing and startup-failed `[device] provider = "fastly"`                                                                                                                                             | **Contradicted by the series, flagged.** The `fastly` device provider ships opt-in with `builtin` (UA-only) as the default (#1044). Whether the host-signal surface stays is sign-off 22, open                                   |
| 10  | Fastly always resolves geo per request                                                                         | Only with `[geo] provider = "platform"`. The neutral default flips only together with the permission model's jurisdiction guard, never in an intermediate step                                                                          | Implemented as sequenced (#1044 kept the platform default, and #1045 flipped geo off by default together with `default_country` and the acknowledgment guard)                                                                    |
| 11a | Raw EC egress on jurisdiction-gated paths today (`user.id`, EIDs, identify, pull sync)                         | Gated by the sharing pair (storage plus personalised-ads), at least as strict as today for every path                                                                                                                                   | Implemented (#1045), where `ec_sharing_allowed` gates `user.id`, the identify response, and pull sync, and `gate_eids_by_permissions` gates EIDs, all on the same pair                                                           |
| 11b | Proxy / click / Testlight forwarding extract the raw EC cookie/header without today's jurisdiction gate        | Gated by the egress inventory (both purposes)                                                                                                                                                                                           | Open. Not implemented by the series (sign-off 8)                                                                                                                                                                                 |
| 11c | Batch sync only authenticates the S2S caller and checks row state                                              | Gated by stored-provenance recompute, with legacy rows failing closed until backfilled                                                                                                                                                  | Open. Not implemented. Batch sync still checks caller and row state, with missing or withdrawn rows collapsing to ineligible (sign-offs 7, 13, 25)                                                                               |
| 12  | EC generation succeeds without a configured identity-graph store                                               | The draft required an openable graph store at startup for a creating provider                                                                                                                                                           | **Changed from the draft (#1043/#1046).** No startup requirement ships. The implemented rule is per request, where no available graph means no identifier is created (no phantom cookies), organic and resolve paths alike       |
| 13  | Cookies created by graphless deployments have no graph row                                                     | Rowless proof under the graphless-migration flag, capped `w` records, disclosed cookie-only expiry                                                                                                                                      | Open. None of the rowless machinery is implemented. A cookie without a row is simply never shared (sign-offs 21, 29, 30)                                                                                                         |

Rows 3, 4, and 7 are policy decisions, not code decisions. In the
implemented series they live in `permissions.yaml`, which is compiled into
the build precisely so the policy stays visible and reviewable in version
control, made explicitly by maintainers rather than implied by an
implementation. The shipped defaults for rows 3 and 4 are flagged above for
the task force.

## 3. Identity stability guarantee

Today's EC identifier is `{64-hex}.{6-char}` where the 64-hex part is
deterministic, `HMAC-SHA256(passphrase, normalized_ip)`, and the 6-char
suffix is random per creation (an existing test asserts two creations
differ). Full identifiers are therefore not reproducible by design, and no
test may pretend otherwise. What stability means, precisely, for a
deployment that selects `provider = "hmac"` and carries its passphrase over
verbatim, with where the series stands on each:

- **The deterministic prefix is stable per inputs.** Implemented and
  tested (#1043,
  `generate_hmac_ec_id_is_stable_per_parts_and_collision_resistant`).
  The committed known-answer vector the draft required (fixed passphrase
  plus IP → exact expected 64-hex prefix, failing CI on divergence) is
  **not yet committed** and remains open work.
- **Existing cookies stay parseable.** The `is_valid_ec_id` shape check is
  unchanged and the HMAC provider's `accepts_id` delegates to that check,
  so pre-series `ts-ec` values keep resolving and their graph rows (keyed
  through `normalize_id_for_kv`, which lowercases the hash exactly as the
  pre-series normalization did) remain reachable.
- **The hash prefix keeps its semantics.** `ec_hash` remains the 64-hex
  prefix, preserving both its stability and its deliberate collision
  across identifiers created from the same IP.
- **Cookie name, attributes, and max-age are unchanged.** `ts-ec`,
  `Domain` derived from the publisher domain, `Path=/`, `Secure`,
  `SameSite=Lax`, `HttpOnly`, one-year `Max-Age` (`ec/cookies.rs`).
- **New global bounds (#1043), a declared addition.** Every identifier,
  whatever provider created the value, must fit 256 bytes and
  `[A-Za-z0-9._~-]`. Out-of-bounds identifiers are rejected loudly and
  never rewritten.

## 4. Configuration migration

Old shape:

```toml
[ec]
passphrase = "replace-with-32-plus-byte-random-secret"
```

New shape:

```toml
[ec]
provider = "hmac"

[ec.providers.hmac]
passphrase = "replace-with-32-plus-byte-random-secret"
```

Requirements, each marked with its implementation state:

1. **The transition has a dual-read release, and loud rejection comes one
   release later.** Implemented (#1043) in the simple form, where the
   current release accepts the old shape, mapping `[ec] passphrase` to the
   `hmac` provider internally and logging a deprecation warning per
   startup, and accepts the new shape. A configuration mixing the two
   forms is rejected, not reconciled. Ordering remains strictly
   reader-first, because pre-series binaries reject the new `[ec]` keys
   and the new `[device]` / `[geo]` sections as unknown fields, so a fleet
   converges binaries first and flips configuration second. The follow-on
   release that rejects `[ec] passphrase` outright with a message naming
   the new location is scheduled, not yet in the tree.

   The draft's full N+1/N+2 interim (the negative-record read/write
   matrix, the `permissions_v2` model promotion, the `m00` mirror, and the
   rollback-floor machinery) is **deferred** together with the durable
   suppression design that motivates all of that machinery (sign-off rows
   11, 16, 19, 20). The series needs none of that machinery because the
   only durable negative artifact today is the identity-graph withdrawal
   tombstone the pre-series lifecycle already wrote, so a binary rollback
   strands no new state. The interim matrix stays recorded in row 20 as
   the design bar for when durable suppression ships.

2. **Adapter qualification is a pre-ratification prerequisite.**
   **Deferred.** No machine-readable capability matrix exists. The
   series' de facto posture is that the identity endpoints (resolve,
   identify, batch sync) route only on the Fastly adapter, and the other
   adapters run without them, which is the stateless-identity path of
   requirement 3. PSL vendoring and the pinned GPP corpus (draft
   requirement text) remain prerequisites of the response-hook and GPP
   follow-ups respectively (sign-offs 23, 28, 32).
3. **Revocation-eligible storage is a per-adapter gate, and ungated
   adapters migrate stateless.** Direction implemented, since
   `provider = "none"` spells explicit statelessness (#1043) and
   non-Fastly adapters deliberately do not route the identity endpoints
   (#1046), matching identify and batch sync. The formal per-adapter
   capability gate is deferred with requirement 2 (sign-off 12).
4. **The graphless migration.** **Superseded in the series.** No
   graphless flag, stub backfill, or rowless classification exists, and
   the draft's 4b startup requirement (a creating provider must have an
   openable graph store at boot) did not ship. The implemented rule is
   request-scoped, where no available identity graph means no identifier
   is created, on the organic path and on resolve alike, so a graphless
   deployment runs identity-less rather than failing startup (§2 row 12).
   The rowless design returns, if at all, with sign-offs 21, 29, and 30.
5. **The graph schema change is expand-contract.** **Deferred.** Rows
   carry the existing schema. No provider/version field, per-permission
   provenance, policy revision, family ID, model epoch, or rollback floor
   ships in the series. This work belongs to the durable-suppression and
   provenance follow-up (sign-offs 11, 16, 19, 20, 25).
6. **Half-migrated fails loud.** Implemented (#1043). An
   `[ec.providers.hmac]` block with no `provider = "hmac"` selector is a
   startup error, as is a selector whose block is absent and an
   unreferenced block alongside a different selection. The exact state
   that validated green and silently created zero ECs in PR #838 now
   refuses to start.
7. **PR #838-era keys.** **Revised.** The draft required rejecting
   `provider = "host-signals"` and `provider = "client-fixed"` as unknown
   keys. The series instead ships both deliberately. `host-signals` is a
   supported opt-in selection (#1044) pending the sign-off 22 review, and
   `client-fixed` exists only under the `client-fixed-demo` cargo feature
   with production builds rejecting the selection at startup (#1046).
   Genuinely unknown keys still fail loud through the unknown-selector
   error and `deny_unknown_fields`.
8. **Provider switches go through legacy readers.** **Deferred.** No
   `legacy_providers` mechanism exists. Today a provider switch on a
   deployment with live identities strands the outgoing provider's
   cookies (the new provider's `accepts_id` rejects them and identity
   restarts). The reader-chain design remains the bar for when a second
   server-side provider makes switching real.
9. **The example config ships the migrated shape.** **Revised.** The
   example ships Edge Cookie identity off by default, with each selector
   and its block documented together and commented together, and
   `default_country = "FR"` uncommented. The draft wanted the happy path
   uncommented. The series instead closes PR #838's silent-stateless trap
   by validation (requirement 6), so a half-uncommented configuration
   refuses to start rather than running stateless. Static-geo examples
   pair `default_country` with the commented
   `assume_single_jurisdiction` acknowledgment, which startup enforces
   whenever an EC provider runs with no geo provider (#1045).
10. Every misconfiguration in the providers spec §6 table fails at
    **startup**. Implemented for configuration errors (#1043-#1045:
    selector/block mismatches, unknown keys, unknown selector values,
    missing `default_country`, missing acknowledgment, demo provider in a
    production build). The pre-series passphrase minimum and placeholder
    rejection carry over unchanged. One residual is declared. A
    selected vendor or host provider the running adapter does not inject
    fails per request, loudly, because adapter injection is a build fact
    the settings layer cannot see. Closing that residual belongs to the
    capability-matrix follow-up (requirement 2).
11. Validation is split into two named layers. Partially implemented.
    Structural validation runs at `ts config push` / `ts config validate`
    (the CLI deserializes and validates the typed settings, with a
    regression test covering environment overlays) and again at startup,
    and startup additionally validates deployment facts only startup can
    see (provider availability in the build, `default_country` against
    the compiled `permissions.yaml`, the acknowledgment rule). The
    machine-readable adapter capability profile for push-time deployment
    pre-checks is deferred with requirement 2.

## 5. Minimal-divergence migration recipe (operator-facing)

"Keep exactly today's behavior" is not fully achievable, and the recipe's
name says so. The divergences that actually shipped in the series, each a
matrix row: global honoring of mapped opt-outs (row 8), an authoritative
TCF refusal blocking new grants everywhere (row 6), the protective
geo-failure floor (row 5), country-wide protective US handling (row 3f),
malformed-present blocking acquisition (row 3c), and the shipped `granted`
US and GB storage baselines (rows 3 and 4, flagged for ratification).
Divergences the draft listed that have **not** shipped and remain deferred:
the sharing/targeted GPP fields (row 3e), the grant-signal class (row 3a,
now stricter instead), and the batch-sync provenance gate (row 11c).

Policy now lives in `permissions.yaml`, compiled into the build. There is
no `[permissions]` block in `trusted-server.toml`, so the draft's
partial-policy trap (a TOML table containing only a permissive default)
cannot be written at all. A deployer edits the yaml and rebuilds, which
keeps every policy change reviewable in version control. The shipped
default policy maps the EU-27 plus the EEA states to `gdpr-eu` (a signal
required for every modeled use), GB to `gdpr-uk` (storage `granted`,
flagged in row 4), and US and AU to `us-opt-out` (a `granted` baseline
where all granted uses drop on any opt-out signal), leaves every unmodeled
Data Use `denied`, and sends unmatched requests to the rule named by the
required `[geo] default_country`.

The migrated operator configuration is:

- `[ec] provider = "hmac"` with its `[ec.providers.hmac]` block (32-plus
  character passphrase), carried over verbatim for identity stability
  (§3);
- `[device]` left at the `builtin` default, with `fastly` as the opt-in
  documented alongside the open sign-off 22 question;
- `[geo] provider = "platform"` where the adapter supplies a host lookup
  (Fastly and Cloudflare in production, the Axum dev server in
  development, while Spin resolves nothing either way). A selected
  provider's lookup failure resolves at the requires-signal floor and
  never at `default_country`. A static deployment instead sets
  `default_country` together with `assume_single_jurisdiction = true`;
- `[geo] default_country` naming the fallback rule (required).

The draft's committed per-adapter fixture files
(`docs/guide/fixtures/migration-preserving-<adapter>.toml`, CI-validated
and pinned against the §2 preservation rows) are **not yet written** and
remain the bar for the migration guide (§7). The guide must also state
that no recipe preserves row 8, because the global honoring of opt-out
signals is unconditional in the shipped model.

## 6. Rollout sequence and observability

1. Implementation PRs land in the epic's order. **Done as specified.**
   The series landed providers first with the geo default held at today's
   behavior (#1044), and the permission model PR flipped the default
   together with its jurisdiction guard (#1045). Each PR's description
   states what that PR changes.
2. **Adapter qualification as a release gate.** **Deferred** with §4
   requirement 2. The series' posture is the declared stateless-identity
   path for adapters without the identity endpoints. The response hook
   remains outside the series entirely (#1047).
3. **Staged activation for policy publication.** **Deferred** (sign-off
   19). `ts config push` today publishes the operator configuration as a
   blob envelope and validates the configuration. The immutable
   `push_sequence` envelope, prepare/commit activation, admission lease,
   quiescence barrier, and scheduled-unavailability protocol are not
   implemented. Policy in the shipped series changes by rebuilding
   (`permissions.yaml`) or by pushing configuration, both taking effect
   on restart/reload rather than through a fleet-wide activation CAS.
4. Before/after deploy, operators watch **EC issuance rate** and EID
   attachment rate as the canary metrics, because the failure mode of a
   bad migration is a silent drop to zero (or a silent grant to
   everyone), not an error rate. **The named metric set with thresholds,
   windows, and actions is not yet built.** The requirement stands for
   the migration guide. The retirement-readiness bar for a legacy
   provider (legacy-reader hits at zero for a quiet period no shorter
   than the maximum cookie/row lifetime plus rollout skew) transfers to
   the deferred `legacy_providers` design (§4 requirement 8).
5. Startup logs. Partially implemented. Startup logs the effective
   default baseline and the exact list of permissions granted without a
   signal (`Permission baseline: [geo] default_country = ...; granted
without a signal: [...]`), plus the passphrase deprecation warning.
   The single greppable line naming the selected provider per concern and
   whether geo is live remains to add.
6. **The batch-sync coverage dip.** **Deferred** with the provenance
   gate (row 11c, sign-offs 7, 13, 25). Today batch sync authenticates
   the caller and checks row state, and rows that are missing or
   withdrawn collapse to ineligible. There is still no fail-open
   shortcut, and grandfathering pre-epic identities past the permission
   model remains rejected.
7. Rollback is config-only where possible. Implemented in the simple
   sense, where selection is configuration, so reverting to the previous
   configuration on the previous binary restores the previous behavior,
   and the same compiled binary switches providers through the
   `TRUSTED_SERVER__ec__provider` / `TRUSTED_SERVER__device__provider` /
   `TRUSTED_SERVER__geo__provider` overrides applied when the operator
   publishes configuration through `ts config push`. The durable
   artifacts today are exactly the **identity-graph withdrawal
   tombstones** written by explicit storage withdrawal (no recovery, that
   is their purpose). The draft's longer durable list (model epoch tuple,
   use-opt-out suppressions, negative outbox, safety breaker) describes
   the deferred durable-suppression design (sign-offs 11, 16, 19, 20).
   The two documented-not-automated procedures (cleanup after a policy
   tightening, legacy-reader retirement) remain for the migration guide.

### 6.1 Operator CLI delta for this epic

The pre-existing CLI design remains unchanged and is what the series
uses. `ts config push` validates and publishes the operator configuration
(with typed environment overlays covered by a CLI regression test), and
`ts config validate` runs the same structural validation standalone.

The draft's normative delta to `ts config push` (never-reused
`push_sequence`, immutable envelope identity, candidate CAS, §5.5
promotion protocol) and the `ts config gc` command are **not
implemented**. They are deferred together with sign-off 19 and remain the
design bar for the durable-suppression and staged-activation follow-up.
Nothing in the series authorizes a generic raw metadata command or a
model-transition command.

## 7. Documentation deliverables

- Migration guide page (§5), linked from `CHANGELOG.md` and the release
  notes. **Outstanding.** The series updated `configuration.md`,
  `edge-cookies.md`, `ec-setup-guide.md`, and `error-reference.md`
  (#1047), and the example configuration documents every selector, but
  the dedicated migration page with per-adapter fixtures and the
  CHANGELOG link do not exist yet.
- `configuration.md` documents **every** valid `provider` value for all
  three concerns. **Done** (#1047), including the required
  `default_country`, the acknowledgment flag, and the requires-signal
  floor on a failed lookup. The environment-variable overrides are now
  real in production, applied as typed EdgeZero app-config overlays when
  the operator publishes through `ts config push`, with a CLI regression
  test. (In PR #838 the documented override existed only under
  `#[cfg(test)]`. The core test-only helper survives as legacy, and the
  runtime path does not use that helper.)
- The permission model page states the precedence rules the code
  implements. **Done** (#1045/#1047). The permission-model guide is in
  the docs navigation, and the precedence is fixed in code (opt-out over
  TCF, malformed-present fail-closed, then TCF), with pinning tests per
  opt-out source against a consenting TCF record. Operator docs and
  normative spec must not diverge on precedence.

## 8. Product decisions requiring explicit sign-off

These are product decisions this spec set needs that #838 had not already
made (or made differently). The table records the recommended resolution
approved for this spec revision, not a final product decision.
**Implementation is blocked while any row is `open`.** Each row is a
decision, not an assignment, since who decided is captured inside the
record itself (`docs/superpowers/specs/decisions/NN-title.md`, the
decision, the deciders, the date). The Decision-record column holds the
link (`(none)` while open). An unratified row reverts to open, not to
silently implemented. **The implemented series (PRs #1043-#1047) is
presented to the task force for ratification of the rows marked below as
implemented or bearing on the series**, so the task force can ratify
rather than re-litigate. Ratifying such a row creates its decision record
and closes the row. Rejecting one reverts the implementation, never keeps
the code with the row open.

| #   | Recommended resolution                                                                                                                                                                                                                                                                                                                    | Where                                       | Decision record | Status                                                                                                                                                                                                                                                      |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------- | --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Honor mapped use opt-outs globally. Destructive identity effects are limited to explicit storage withdrawal, authenticated deletion, or a qualifying live TCF Purpose 1 refusal.                                                                                                                                                          | permission §4, §4.2                         | (none)          | open. Implemented by #1045 (jurisdiction-free suppression, withdrawal only on a live TCF storage refusal under a non-granted baseline). Ratify                                                                                                              |
| 2   | GPP/USP sale opt-outs suppress the personalised-ads uses only. They neither revoke storage nor delete the identity.                                                                                                                                                                                                                       | permission §4.5                             | (none)          | open. Partially implemented by #1045 (no opt-out deletes or destructively revokes). The shipped default maps opt-outs to `revokes: all` rather than personalised-ads only, a `permissions.yaml` choice to ratify                                            |
| 3   | Sharing/targeted-advertising opt-outs suppress the personalised-ads uses. An explicit applicable not-opted-out value may grant them. Neither affects storage.                                                                                                                                                                             | permission §4.5                             | (none)          | open. Not implemented (the sharing and targeted-advertising GPP fields are not decoded, and no US signal grants anything in the shipped model)                                                                                                              |
| 4   | US auction dispatch may continue while personalised-ads is unset only through permission §7's positive `ContextualAuctionView` and its sole normative inline manifest.                                                                                                                                                                    | permission §7.1                             | (none)          | open. The strip half is implemented by #1045 (EIDs and `user.id` withheld when the pair is unset). The positive contextual projection is not implemented                                                                                                    |
| 5   | Country-only and regionless US traffic use a protective country-wide `us-opt-out` floor. State rules may be stricter.                                                                                                                                                                                                                     | permission §3.4                             | (none)          | open. Implemented by #1045 (`permissions.yaml` maps country `US` to `us-opt-out` with state overrides available). Ratify                                                                                                                                    |
| 6   | Raw regulatory strings reach only the positively registered OpenRTB field that requires each source. All other destinations default deny. Identity rows retain normalized provenance/digests, not raw consent snapshots.                                                                                                                  | permission §7; providers §6.3               | (none)          | open. Not addressed by the series                                                                                                                                                                                                                           |
| 7   | Reject legacy batch-sync traffic until live-browser provenance backfill makes the row re-evaluable.                                                                                                                                                                                                                                       | rollout §6 item 6; permission §7            | (none)          | open. Not implemented (no provenance exists to recompute)                                                                                                                                                                                                   |
| 8   | Gate proxy, click, and Testlight identity forwarding on the sharing pair (storage plus personalised-ads).                                                                                                                                                                                                                                 | §2 row 11b                                  | (none)          | open. Not implemented by the series                                                                                                                                                                                                                         |
| 9   | Defer integration-owned cookie operations from the v1 response hook, and require a complete read/use/withdraw lifecycle before admission.                                                                                                                                                                                                 | hook §3                                     | (none)          | open. Overtaken (#1047 removed the whole hook from the series, so no cookie surface shipped). The deferral returns with the hook's first consumer                                                                                                           |
| 10  | Do not create a blanket session-cookie exemption. Every cookie must be covered by an approved permission or narrowly defined security-use authority.                                                                                                                                                                                      | hook §3                                     | (none)          | open. Hook not shipped, unaffected                                                                                                                                                                                                                          |
| 11  | Require a durable per-family negative-intent outbox in a failure domain independent of its strong target and checked freshly by every identity consumer, with a globally visible breaker over positive identity operations when neither can commit.                                                                                       | permission §4.3                             | (none)          | open. Not implemented (part of the durable-suppression follow-up)                                                                                                                                                                                           |
| 12  | Adapters that cannot meet the revocation-storage contract migrate stateless rather than weakening the contract.                                                                                                                                                                                                                           | rollout §6 item 2; recipe §5                | (none)          | open. Direction implemented (`provider = "none"` spells stateless in #1043; the identity endpoints were already Fastly-only before the series, and #1046 keeps `resolve` on the same footing). The formal capability gate is deferred. Ratify the direction |
| 13  | Keep batch sync fail-closed at cutover, and stage partner communication and cleanup using explicit coverage thresholds, windows, and pause actions.                                                                                                                                                                                       | rollout §6 item 6                           | (none)          | open. Not implemented (no provenance cutover exists yet)                                                                                                                                                                                                    |
| 14  | Policy tightening does not reinterpret historical refusal as a destructive event. Destructive withdrawal requires fresh, live qualifying evidence.                                                                                                                                                                                        | permission §4.2 trigger 2                   | (none)          | open. Implemented by #1045 (withdrawal evaluates the live request's TCF record only, and historical records are never reinterpreted). Ratify                                                                                                                |
| 15  | Descope the client cycle and `rewrite_legacy`, and ship the v1 integration hook as headers-only.                                                                                                                                                                                                                                          | client spec status; providers §6.1; hook §3 | (none)          | open. Overtaken (the client cycle shipped hardened as #1046 instead of descoped, `rewrite_legacy` does not exist, and the hook shipped not at all per #1047). Re-decide against the shipped shape                                                           |
| 16  | Persist use-opt-out suppression until ordered explicit authorization for that use or identity deletion, with TCF `LastUpdated` or an authenticated monotonic revision proving order.                                                                                                                                                      | permission §4.3                             | (none)          | open. Not implemented (suppression in the series is request-scoped with no durable record)                                                                                                                                                                  |
| 17  | N/A, absent, reserved, unknown, and unsupported values never grant processing.                                                                                                                                                                                                                                                            | permission §4.5                             | (none)          | open. Implemented by #1045 in a stricter form (signals only revoke, so no US-signal value grants anything). Ratify                                                                                                                                          |
| 18  | A selected geo provider's lookup failure uses the compiled-in protective profile; `default_country` is only for acknowledged static-jurisdiction mode.                                                                                                                                                                                    | permission §5.2                             | (none)          | open. Implemented by #1045 (failed lookups resolve at the requires-signal floor, logged at error level, never `default_country`). Ratify                                                                                                                    |
| 19  | Use immutable version-addressed whole-config publication plus prepare/commit activation of the complete tuple, with authenticated fleet membership, bounded admission lease, quiescence, and an activation journal. A second unanimous model transition advances model epoch, minimum binary generation, and row schema floor atomically. | permission §5.5; rollout §6.1               | (none)          | open. Not implemented (`ts config push` publishes and validates without the activation protocol)                                                                                                                                                            |
| 20  | N+1 keeps v1 creation and pre-epic live gating, reads/enforces N+2 negative state for rollback safety, and never originates durable use suppression. New-shape settings alone do not activate the new writer/model.                                                                                                                       | migration §4.4                              | (none)          | open. Overtaken in part (the shipped migration is a one-release dual-read of `[ec] passphrase` in #1043 with mixed forms rejected and nothing durable added, so the full interim waits for the durable design)                                              |
| 21  | Expire and re-create rowless legacy cookies without continuity. A prefix match cannot authenticate the cookie suffix.                                                                                                                                                                                                                     | providers §5                                | (none)          | open. Not implemented (no rowless classification exists, and a cookie with no row is never shared)                                                                                                                                                          |
| 22  | Defer host JA4/H2 signal processing to a separate approved design. Reject `[device] provider = "fastly"` at startup and do not persist signal-derived classifications.                                                                                                                                                                    | providers §5                                | (none)          | open. **Contradicted by the series and flagged for review** (#1044 ships the `fastly` device provider and the host-signal EC provider opt-in, and device signals including signal hash prefixes persist in rows)                                            |
| 23  | Permit a narrow `SecurityUse` authority for DataDome only, with the exact bounded surface the hook spec defines.                                                                                                                                                                                                                          | hook §4a; permission §7                     | (none)          | open. Hook not shipped, unaffected                                                                                                                                                                                                                          |
| 24  | Malformed/absence suppression overrides a permissive baseline but clears on newer valid evidence. It is not sticky like an explicit use opt-out.                                                                                                                                                                                          | permission §4.3, §4.1                       | (none)          | open. Implemented by #1045 by construction (the fail-closed block is request-scoped, so newer valid evidence re-resolves). Ratify together with row 16's durable design                                                                                     |
| 25  | Enforce a stored-jurisdiction/provenance horizon for batch sync, where moves into stricter regimes fail closed by the horizon, and moves out require a live visit.                                                                                                                                                                        | permission §7                               | (none)          | open. Not implemented                                                                                                                                                                                                                                       |
| 26  | Aggregate embedded GPP GPC with `Sec-GPC` by OR as a global, non-destructive use opt-out.                                                                                                                                                                                                                                                 | permission §4.5                             | (none)          | open. Partially implemented by #1045 (`Sec-GPC` is an honored non-destructive source). The embedded GPP GPC subfield is not read                                                                                                                            |
| 27  | In proxy mode, decode only mapped opt-out fields and derive no grants.                                                                                                                                                                                                                                                                    | permission §4.4                             | (none)          | open. Partially (the shipped model derives no grants from any US signal anywhere). The proxy-mode decode restriction is not separately implemented                                                                                                          |
| 28  | Require product and written vendor conformance approval for the reduced DataDome surface the hook spec pins.                                                                                                                                                                                                                              | hook §4a.2                                  | (none)          | open. Hook not shipped, unaffected                                                                                                                                                                                                                          |
| 29  | Accept rowless roaming-cookie expiry as a bounded residual only with telemetry, an explicit maximum lifetime, operator documentation, and a removal/sunset criterion.                                                                                                                                                                     | providers §5                                | (none)          | open. Not implemented                                                                                                                                                                                                                                       |
| 30  | Saturation blocks rowless admission for that prefix but never revokes an authenticated real row without its exact suffix. Monitor NAT-cohort pressure.                                                                                                                                                                                    | providers §5                                | (none)          | open. Not implemented                                                                                                                                                                                                                                       |
| 31  | Keep replay history bounded by evicting expired/grant entries first and retaining restrictive state for its full horizon. Saturation never shortens a later opt-out.                                                                                                                                                                      | permission §4.3; providers wire schema      | (none)          | open. Not implemented                                                                                                                                                                                                                                       |
| 32  | Accept official GPP sections 24-27 version 1, pin their layouts to the vendored IAB commit, and treat complete decoder/fixture support as a release prerequisite, with full vendoring evidence.                                                                                                                                           | permission §4.5.1                           | (none)          | open. Not implemented (the shipped decoder consults the GPP sale field, with no vendored IAB corpus)                                                                                                                                                        |
| 33  | Treat any malformed or unsupported-version **mapped** GPP section as a global blocker for grants to the permissions its schema maps, while still honoring decodable opt-outs elsewhere and never deriving withdrawal from malformed bytes. Unknown unmapped section IDs remain non-contributing.                                          | permission §4.5                             | (none)          | open. Partially implemented by #1045 (a present undecodable record blocks baseline grants). The per-section mapped-blocker rule is not implemented                                                                                                          |
| 34  | Permit providers whose canonical identifiers cannot fit an injective graph suffix to use the `sha256-detect` mode: domain-separated collision resistance plus stored canonical-identifier comparison, fail-closed collision handling, no overwrite/join.                                                                                  | providers §2, §6.3                          | (none)          | open. Premise revised by #1043 (identifiers are globally bounded at 256 bytes and the graph is keyed by `normalize_id_for_kv`). No `sha256-detect` mode exists                                                                                              |

## Revision record vs the 2026-07-31 draft

| Draft position                                                                                        | Revised position                                                                                                                                                                                                                                                                                                                                                                                     | Why                                                                                                                                                          |
| ----------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Epic #777-#781 pending sign-off, no implementation                                                    | Implemented as five stacked PRs #1043-#1047, with §8 staying the ledger and rows marked for ratification                                                                                                                                                                                                                                                                                             | The series shipped the seam, selection, permission model, resolve endpoint, and docs, so the task force ratifies rather than re-litigates                    |
| `[device] provider = "fastly"` startup-fails pending a separate host-signal design (§2 row 9, row 22) | The `fastly` device provider and host-signal EC provider ship opt-in, and device signals persist in graph rows. The identifier-collision defect the review found (host-signal identifiers shared the HMAC grammar and keyspace) is fixed by the mandatory provider-code envelope, so host-signal identifiers are `hs00~` namespaced. The policy question of row 22 is unchanged by the collision fix | Implementation choice, deliberately flagged as the open review question rather than presented as settled                                                     |
| US recipe: `requires_signal` with an extended grant-signal class (§2 rows 3/3a)                       | Shipped default: `granted` US baseline, `revokes: all` on any opt-out, and no US signal grants anything                                                                                                                                                                                                                                                                                              | The shipped signal model is revoke-only, which is simpler and stricter on grants, and the baseline choice is deployer-editable yaml, flagged in rows 3 and 5 |
| `[permissions]` TOML policy published at runtime                                                      | `permissions.yaml` compiled into the build, no `[permissions]` block                                                                                                                                                                                                                                                                                                                                 | Policy stays reviewable in version control and the partial-policy trap cannot be written                                                                     |
| `rules.default` worldwide default entry (§2 row 7)                                                    | Required `[geo] default_country` naming the fallback rule, validated at startup                                                                                                                                                                                                                                                                                                                      | Same role, one mechanism, loud when missing                                                                                                                  |
| Graph store mandatory at startup for creating providers (§2 row 12, §4.4b)                            | No startup requirement, and with no graph no identifier is created, per request                                                                                                                                                                                                                                                                                                                      | The phantom-cookie rule holds without a breaking startup change, and graphless deployments run identity-less                                                 |
| `legacy_providers` reader chain for provider switches (§4.8)                                          | Not implemented, so a switch restarts identity                                                                                                                                                                                                                                                                                                                                                       | Deferred until a second server-side provider makes switching real                                                                                            |
| N+1/N+2 negative-record machinery, model epochs, `m00` mirror (§4.1, row 20)                          | Deferred with rows 11, 16, 19, 20. The shipped dual-read is one release of `[ec] passphrase` mapping with mixed forms rejected                                                                                                                                                                                                                                                                       | Nothing durable beyond pre-existing withdrawal tombstones ships, so binary rollback strands no new state                                                     |
| Staged activation, `push_sequence`, quiescence, `ts config gc` (§6.3, §6.1)                           | Basic `ts config push` / `ts config validate` only                                                                                                                                                                                                                                                                                                                                                   | The activation protocol belongs to the deferred durable-suppression rollout (row 19)                                                                         |
| Migration guide with committed per-adapter fixtures and a full gated metric set (§5, §6.4)            | `configuration.md` / `edge-cookies.md` document the migrated shape, while fixtures, the guide page, and metrics remain outstanding                                                                                                                                                                                                                                                                   | Documentation shipped for configuration, and the operational guide is the remaining deliverable before a release                                             |
| Client cycle descoped and hook shipped headers-only (row 15)                                          | The client cycle shipped hardened (#1046) and the hook shipped not at all (#1047)                                                                                                                                                                                                                                                                                                                    | Hardening replaced descoping, and the hook had no consumer, which is the hook spec's own admission rule                                                      |
| Example config ships the migrated happy path uncommented (§4.9)                                       | Identity off by default with selector and block commented together, and validation closes the silent-stateless trap                                                                                                                                                                                                                                                                                  | Loud startup validation, not an uncommented default, is what prevents PR #838's silent-stateless state                                                       |
| Environment override documented but `#[cfg(test)]`-only in PR #838 (§7)                               | Override applied as typed EdgeZero app-config overlays at `ts config push`; the existing CLI overlay test covers the mechanism, and a provider-specific override test is still to write                                                                                                                                                                                                              | The same compiled binary switches providers at deployment through the published configuration                                                                |
| Pinned known-answer HMAC vectors committed (§3)                                                       | Stability tested per inputs, and the pinned vector is still to commit                                                                                                                                                                                                                                                                                                                                | The cross-version CI pin remains open work under §3                                                                                                          |
| GB storage baseline change only with citation and sign-off (§2 row 4)                                 | The shipped `permissions.yaml` adopts `granted` storage for GB without a recorded citation                                                                                                                                                                                                                                                                                                           | Flagged in row 4, since the task force owns the decision and its record                                                                                      |
