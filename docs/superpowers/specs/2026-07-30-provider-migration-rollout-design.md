# Design Spec: Provider and Permission Model — Migration and Rollout

**Status:** Draft
**Author:** Engineering
**Issue references:** #777–#781 (epic)
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-permission-model-design.md`
**Last updated:** 2026-07-31

> **Context.** The provider/permission epic is a breaking change to a live
> identity system. PR #838's review showed that the riskiest part of such a
> change is not the new code but the transition: silent misconfiguration
> modes, undeclared behavior changes discovered by deleted tests, and no
> written statement of which pre-change behaviors were guaranteed to
> survive. This spec is that statement. Any implementation PR in the epic
> must reconcile its diff against §2's matrix and list every deliberate
> divergence in its description.

---

## 1. Scope

Covers the transition of existing deployments from the hard-wired EC /
device / geo behavior to the provider architecture and permission model.
Applies to every implementation PR in the epic, and to the operator-facing
migration guide that ships with the last of them.

## 2. Behavior-preservation matrix

For each decision the system makes today, the target behavior after the epic,
and whether that is a preservation or a declared change. **Silent changes are
defects.** PR #838 changed six of these without declaring any; each was
discoverable only because a deleted test had pinned the old behavior.

| #   | Decision (today)                                                                                                                                                                    | After epic                                                                                                                                                                                                                                                        | Status                                                                                                |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| 1   | EU/GDPR request, no TCF consent → no EC                                                                                                                                             | Same (opt-in baseline)                                                                                                                                                                                                                                            | Preserved                                                                                             |
| 2   | US-state request, GPC/GPP/USP opt-out → no EC, existing EC expired + tombstoned, **even when a consenting TCF string is present**                                                   | Same (precedence §4 of permission spec)                                                                                                                                                                                                                           | Preserved — **must not regress**                                                                      |
| 3   | US-state request, no signals at all → no EC (fail-closed)                                                                                                                           | Preserved by the recipe: the US rule is `requires_signal`, and the extended grant-signal class (permission spec §4) is what makes that possible — `granted` would allow no-signal traffic; a TCF-only grant class could not grant from GPP/USP values             | Preserved under the recipe                                                                            |
| 3a  | US-state request, explicit GPP `sale_opt_out = false` or a US Privacy string that is present and not opting out (including "not applicable") → EC allowed                           | Same: these are grant-class signals satisfying `requires_signal` (permission spec §4)                                                                                                                                                                             | Preserved — **must not regress**                                                                      |
| 3b  | US-state request, TCF record present and refusing Purpose 1 (no US opt-out signal) → no EC                                                                                          | Same: refusal beats coexisting non-TCF grant signals (permission spec §4, precedence 3–4)                                                                                                                                                                         | Preserved                                                                                             |
| 3c  | Consent-record conflict modes (restrictive/permissive/newest), expiry, KV fallback, proxy mode                                                                                      | Each row of the normalization matrix (permission spec §4.4) is individually marked preserved or changed there; changed rows: malformed-present now blocks acquisition                                                                                             | Per §4.4 matrix                                                                                       |
| 4   | UK request, no TCF record → no EC                                                                                                                                                   | Same, unless the policy deliberately adopts a `granted` storage baseline for GB, with citation and sign-off                                                                                                                                                       | Declared change (if made)                                                                             |
| 5   | No country resolvable (geo unavailable) → no EC (fail-closed)                                                                                                                       | `default_country` baseline, constrained by permission spec §5.3 so the fail-open combination cannot occur silently                                                                                                                                                | Declared change, guarded                                                                              |
| 6   | Non-regulated country, TCF record refusing Purpose 1 → EC still created, existing identity never tombstoned                                                                         | Refusal now blocks _new_ grants everywhere (permission spec §4, precedence 3) — a declared, more-protective change. Existing identity is still **never tombstoned** where the baseline is `granted` (permission spec §4.2)                                        | Split: creation is a declared change; no-tombstone is preserved                                       |
| 7   | Country resolved but not in any regulation list ("non-regulated") → EC created, EIDs pass through                                                                                   | Governed by the policy's `rules.default` entry (permission spec §5.4). The §5 recipe sets it to a `granted` baseline to preserve today's behavior; the protective example policy instead requires a signal worldwide — a declared operator choice between the two | Preserved under the recipe; declared change under the protective default                              |
| 8   | Opt-out signal (GPC/GPP/USP) **outside** US states → ignored today                                                                                                                  | Revokes and withdraws globally (permission spec §4 and §4.2 trigger 1) — including tombstoning, which is irreversible                                                                                                                                             | Declared change, more protective, **irreversible** — see §6.4                                         |
| 9   | Fastly bot gate requires JA4 + platform class before KV-backed EC writes                                                                                                            | Only with `[device] provider = "fastly"`; the `builtin` default is UA-only                                                                                                                                                                                        | Declared change with a documented restore step (§5)                                                   |
| 10  | Fastly always resolves geo per request                                                                                                                                              | Only with `[geo] provider = "platform"`. The neutral geo default flips **only** in the permission model PR, together with the §5.3 guard — never in an intermediate step where absent geo would fail closed and zero EC issuance (providers spec §11)             | Declared change, sequenced, with a documented restore step (§5)                                       |
| 11a | Raw EC egress on paths gated by the jurisdiction gate today (OpenRTB `user.id`, derived request IDs, page bids, EIDs, identify, pull sync — pull checks the live `EcContext` today) | Gated by the egress inventory (permission spec §7): bidstream and partner egress require both purposes, revocation exempt — at least as strict as today for every path                                                                                            | Preserved (strengthened); **must not regress** — PR #838 gated only EIDs and left `user.id` reachable |
| 11b | Proxy / click / Testlight forwarding extract the raw EC cookie/header **without** today's jurisdiction gate                                                                         | Gated by the egress inventory (both purposes)                                                                                                                                                                                                                     | **New privacy hardening, declared change** — not preservation                                         |
| 11c | Batch sync today only authenticates the S2S caller and checks live/tombstoned row state — it is **not** jurisdiction-gated                                                          | Gated by stored-provenance recompute (permission spec §7); legacy rows fail closed until backfilled                                                                                                                                                               | **New privacy hardening, declared change** — not preservation                                         |

Rows 3, 4, and 7 are policy decisions, not code decisions: they belong in
the `[permissions]` policy review, made explicitly by maintainers — not
implied by an implementation.

## 3. Identity stability guarantee

Today's EC identifier is `{64-hex}.{6-char}` where the 64-hex part is
deterministic — `HMAC-SHA256(passphrase, normalized_ip)` — and the 6-char
suffix is **random per mint** (an existing test asserts two mints differ).
Full identifiers are therefore not reproducible by design, and no test may
pretend otherwise. What stability means, precisely, for a deployment that
selects `provider = "hmac"` and carries its passphrase over verbatim:

- **The deterministic prefix is bit-identical.** Pinned known-answer
  vectors: fixed passphrase + IP → exact expected 64-hex prefix, committed
  so any divergence fails CI rather than rotating the production identity
  base.
- **Existing cookies stay parseable.** Fixture `ts-ec` values minted by
  the pre-epic code pass the provider's `parse`, and their graph rows
  (keyed by the identifier verbatim) remain reachable — no row is orphaned.
- **The hash prefix keeps its semantics.** `ec_hash` remains the 64-hex
  prefix, preserving both its stability and its deliberate collision across
  identifiers minted from the same IP — the property IP-cluster trust
  counting depends on (providers spec §3).
- **Cookie name, attributes, and max-age are unchanged** (the domain
  remains config-derived, as today).

## 4. Configuration migration

Old shape:

```toml
[ec]
passphrase = "example-passphrase"
```

New shape:

```toml
[ec]
provider = "hmac"

[ec.providers.hmac]
passphrase = "example-passphrase"
```

Requirements:

1. **The transition has a dual-read release; loud rejection comes one
   release later.** Today's binary _requires_ `[ec] passphrase` and — via
   `deny_unknown_fields` — _rejects_ `[ec] provider` and
   `[ec.providers.*]`; a binary that rejects the old shape outright would
   mean **no config both binaries accept**, and a config-store fleet
   cannot flip config and binaries atomically. Sequence:
   - **Release N+1 (dual-read):** accepts the old shape (mapping
     `[ec] passphrase` to the `hmac` provider internally, logging a
     deprecation warning per startup) _and_ the new shape. Ordering is
     **strictly reader-first, never "either order"**: current binaries
     reject the new shape (and reject the new `[permissions]` / `[device]`
     / `[geo]` additions as unknown fields), so the config may flip only
     after **fleet convergence on N+1 is confirmed** — binaries first,
     convergence gate, then `ts config push`. A config mixing old and new
     fields (`[ec] passphrase` alongside `[ec] provider`) is **rejected**
     by N+1, not reconciled. Rollback runs the sequence in reverse: config
     back to the old shape first, binaries only after config convergence.
     Every new config section introduced by the epic follows this same
     compatibility rule, not only `[ec]`.
   - **Release N+2:** rejects `[ec] passphrase` at startup with a message
     naming the new location — not a generic unknown-field error
     (implementation note: producing the actionable message means keeping
     a deprecated `passphrase` field whose presence triggers the custom
     error).
2. **The graph schema change is expand-contract, in lockstep with the
   binary sequence.** New rows carry fields v1 rows never had — provider/
   version, per-permission grant evidence, policy revision, family ID,
   rewrite links — and two failure modes must be engineered away: a naive
   schema-version bump makes old readers fail closed on new rows, and an
   old worker that reads, modifies, and reserializes a row **silently
   drops** fields it does not model. Sequence: (a) a **reader/preserver
   release** ships first — it understands the new fields and, critically,
   preserves unknown fields verbatim through read-modify-write; (b) a
   **fleet-convergence gate**; (c) only then does **writer activation**
   begin emitting the new fields. Rows carry an explicit schema version;
   backfill is lazy via live requests (the same pass that backfills legacy
   provenance, permission spec §7). Mixed-version tests are mandatory:
   old-reader/new-row, new-reader/old-row, and old-worker
   read-modify-write preserving new fields byte-for-byte.
3. **Half-migrated fails loud.** A `[ec.providers.hmac]` block with no
   `provider = "hmac"` selector is a startup error (providers spec §6). In
   PR #838 this configuration — the exact state an operator following the
   docs reaches if they miss one line — validated green and silently minted
   zero ECs.
4. **PR #838-era keys fail loud.** `provider = "host-signals"` (shipped by
   PR #838, deliberately not carried into this epic — providers spec §2)
   and `provider = "client-fixed"` are unknown keys and rejected like any
   other, so a config written against the PR #838 example cannot silently
   select a provider that no longer exists.
5. **Provider switches go through legacy readers.** Changing
   `[ec] provider` on a deployment with live identities requires listing
   the outgoing provider in `[ec] legacy_providers` (providers spec §6.1)
   so existing cookies keep resolving and stay withdrawable; the guide
   documents the switch sequence and the retirement/cleanup step that ends
   it.
6. **The example config ships the migrated happy path**, uncommented:
   `provider = "hmac"` with its block, `[geo] default_country`, and (for
   Fastly) the behavior-preserving `[device] provider = "fastly"` and
   `[geo] provider = "platform"` lines present with a comment stating what
   removing them changes. PR #838's example shipped the passphrase block
   uncommented with the selector commented out — steering operators directly
   into the silent-stateless state.
7. Every misconfiguration in the providers spec §6 table fails at
   **startup**. Request-time failure for a configuration error is a defect.
8. Config-store payload validation (`ts config push`) applies the same
   rules — including `[permissions]` policy validation (permission spec
   §3.3) — so a bad config is rejected at push time, before any instance
   restarts into it.

## 5. Behavior-preserving migration recipe (operator-facing)

The migration guide (a new `docs/guide/` page, linked from the release notes)
gives one copy-pasteable recipe per adapter for "keep exactly today's
behavior":

The recipe is a **complete, valid TOML fixture per adapter, committed to
the repository** (e.g. `docs/guide/fixtures/migration-preserving-fastly.toml`
and siblings) and included in the guide verbatim — never described as a
textual delta against the example file. Per-adapter because a single
fixture cannot be: `[device] provider = "fastly"` and
`[geo] provider = "platform"` are capability-gated selections that the
Axum/Cloudflare/Spin adapters reject at startup (providers spec §6); each
adapter's fixture carries the selections valid for it, and each is
CI-validated against its adapter. (An earlier draft said "copy the example table,
then set `[permissions.rules] default`" — but the copied table already
declares `[permissions.rules]`, and reopening a TOML table is a parse
error; a prose delta cannot be validated, a committed fixture can.) The
fixture contains, in one document:

- `[ec] provider = "hmac"` with its passphrase block;
- `[device] provider = "fastly"` (Fastly deployments: preserves the JA4
  bot gate);
- `[geo] provider = "platform"` and `default_country = "FR"` (per-request
  jurisdiction detection preserved; the default is fail-closed because FR
  resolves to the `gdpr-eu` rule);
- (Fastly fixture; other adapters substitute their valid selections)
  the full `gdpr-eu` / `gdpr-uk` / `us-opt-out` groups and country rules
  from the example policy (US as `requires_signal` with the grant-signal
  class — §2 rows 3–3b), plus the `non-regulated` group with
  `rules.default = "non-regulated"` (row 7). Operators who prefer the
  protective worldwide default use the example file itself instead.

A partial policy is a trap the first draft of this spec fell into: a
`[permissions]` section containing **only** the permissive
default — with no GDPR/US rules — sends _every_ jurisdiction, France
included, to the permissive fallback, because `default_country` selects a
rule like any other country and finds none. Each committed fixture is
therefore always complete, and CI pins it: **the fixture file itself** is
loaded and run through the complete §4.1/§4.2 decision matrix of the
permission spec, asserting per-jurisdiction outcomes match the pre-epic
gate for every preservation row of §2.

The guide separately documents the neutral configuration and what it does
_not_ do, states explicitly that `default_country` alone does not replace
geo lookup, why the no-geo combination requires the explicit acknowledgment
flag (permission spec §5.3), and that no recipe preserves row 8 of §2 — the
global honoring of opt-out signals is unconditional.

## 6. Rollout sequence and observability

1. Implementation PRs land in the epic's order (providers spec §11:
   providers first with the geo default held at today's behavior, the
   permission model PR flipping it together with its guard); each PR is
   reviewable against §2 in isolation and states which rows it touches.
2. Before/after deploy, operators watch **EC issuance rate** and EID
   attachment rate; the migration guide names these as the canary metrics,
   because the failure mode of a bad migration is a silent drop to zero (or a
   silent grant to everyone), not an error rate. The full metric set, each
   with a stated healthy range: geo lookup-failure/fallback rate (permission
   spec §5.2), raw-egress denials by path, tombstone family retries,
   legacy-reader hit rate, rewrite failures, and cluster-fallback
   engagements. Two of these carry thresholds, not just ranges:
   legacy-reader hits at zero for a **quiet period no shorter than the
   maximum cookie/row lifetime plus rollout skew** — or provable
   rewrite/backfill completion — is the **retirement-readiness** bar for a
   legacy provider ("trending to ~zero" is not evidence; a yearly visitor
   is not churn), and a nonzero rewrite-failure rate blocks retirement
   outright. The telemetry set also includes: graph read/commit failures,
   stored-provenance denials, schema-migration failures, and
   replay-reservation recoveries.
3. Startup logs always print: selected provider per concern, whether geo is
   live, the effective default baseline, and the count of granted-without-
   signal permissions. One line, greppable, stable format.
4. Rollback is config-only where possible: reverting to the previous
   config version restores the previous behavior on the previous binary. The
   one irreversible artifact is withdrawal tombstones — which is why the
   withdrawal triggers (permission spec §4.2) are exhaustive, why partial
   withdrawal failure has an explicit tombstones-first, browser-retries
   contract (permission spec §4.3), and why §2 rows 6 and 8 call out
   tombstoning explicitly. Two operational procedures are documented in the
   guide, not automated: cleanup of identities minted before a policy
   tightening (permission spec §4.2 trigger 3), and retirement of a legacy
   reader after a provider switch (providers spec §6.1), which is the
   deliberate end of the identities only that reader can resolve.

## 7. Documentation deliverables

- Migration guide page (§5), linked from `CHANGELOG.md` and the release
  notes.
- `configuration.md` documents **every** valid `provider` value for all
  three concerns, the full `[permissions]` schema, and environment-variable
  overrides only if they actually work in production builds (in PR #838 the
  documented `TRUSTED_SERVER__EC__PROVIDER` override existed only under
  `#[cfg(test)]`).
- The permission model page states the §4 precedence rules of the permission
  spec verbatim — operator docs and normative spec must not diverge on
  precedence, and prose like "signals are mapped as a grant or a revoke"
  without stating which wins is insufficient.
