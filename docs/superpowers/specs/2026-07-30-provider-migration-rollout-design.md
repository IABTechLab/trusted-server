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

| #   | Decision (today)                                                                                                                                                       | After epic                                                                                                                                                                                                                                                        | Status                                                                                                |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| 1   | EU/GDPR request, no TCF consent → no EC                                                                                                                                | Same (opt-in baseline)                                                                                                                                                                                                                                            | Preserved                                                                                             |
| 2   | US-state request, GPC/GPP/USP opt-out → no EC, existing EC expired + tombstoned, **even when a consenting TCF string is present**                                      | Same (precedence §4 of permission spec)                                                                                                                                                                                                                           | Preserved — **must not regress**                                                                      |
| 3   | US-state request, no signals at all → no EC (fail-closed)                                                                                                              | Policy decision: the shipped `US` baseline decides. If the baseline grants `store-on-device` without a signal, that is a **declared change** requiring sign-off in the policy review, with rationale in the policy itself                                         | Declared change (if made)                                                                             |
| 4   | UK request, no TCF record → no EC                                                                                                                                      | Same, unless the policy deliberately adopts a `granted` storage baseline for GB, with citation and sign-off                                                                                                                                                       | Declared change (if made)                                                                             |
| 5   | No country resolvable (geo unavailable) → no EC (fail-closed)                                                                                                          | `default_country` baseline, constrained by permission spec §5.3 so the fail-open combination cannot occur silently                                                                                                                                                | Declared change, guarded                                                                              |
| 6   | Non-regulated country, TCF record refusing Purpose 1 → EC still created, existing identity never tombstoned                                                            | Refusal now blocks _new_ grants everywhere (permission spec §4, precedence 3) — a declared, more-protective change. Existing identity is still **never tombstoned** where the baseline is `granted` (permission spec §4.2)                                        | Split: creation is a declared change; no-tombstone is preserved                                       |
| 7   | Country resolved but not in any regulation list ("non-regulated") → EC created, EIDs pass through                                                                      | Governed by the policy's `rules.default` entry (permission spec §5.4). The §5 recipe sets it to a `granted` baseline to preserve today's behavior; the protective example policy instead requires a signal worldwide — a declared operator choice between the two | Preserved under the recipe; declared change under the protective default                              |
| 8   | Opt-out signal (GPC/GPP/USP) **outside** US states → ignored today                                                                                                     | Revokes and withdraws globally (permission spec §4 and §4.2 trigger 1) — including tombstoning, which is irreversible                                                                                                                                             | Declared change, more protective, **irreversible** — see §6.4                                         |
| 9   | Fastly bot gate requires JA4 + platform class before KV-backed EC writes                                                                                               | Only with `[device] provider = "fastly"`; the `builtin` default is UA-only                                                                                                                                                                                        | Declared change with a documented restore step (§5)                                                   |
| 10  | Fastly always resolves geo per request                                                                                                                                 | Only with `[geo] provider = "platform"`. The neutral geo default flips **only** in the permission model PR, together with the §5.3 guard — never in an intermediate step where absent geo would fail closed and zero EC issuance (providers spec §11)             | Declared change, sequenced, with a documented restore step (§5)                                       |
| 11  | Raw EC egress (OpenRTB `user.id`, derived request IDs, page bids, proxy/click/Testlight forwarding, identify, pull/batch sync) is gated by the jurisdiction gate today | Gated by the egress inventory (permission spec §7): bidstream egress requires both purposes, first-party identity operations require `store-on-device`, revocation exempt — at least as strict as today for every inventoried path                                | Preserved (strengthened); **must not regress** — PR #838 gated only EIDs and left `user.id` reachable |

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
- **Existing cookies stay recognized.** Fixture `ts-ec` values minted by
  the pre-epic code pass the provider's `recognize`, and their graph rows
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

1. **Old key fails loud.** `[ec] passphrase` is rejected at startup with a
   message naming the new location — not a generic unknown-field error.
   Implementation note: `Ec` already carries `deny_unknown_fields`, which
   would reject the key generically; producing the actionable message means
   keeping a deprecated `passphrase` field whose presence triggers the
   custom error.
2. **Half-migrated fails loud.** A `[ec.providers.hmac]` block with no
   `provider = "hmac"` selector is a startup error (providers spec §6). In
   PR #838 this configuration — the exact state an operator following the
   docs reaches if they miss one line — validated green and silently minted
   zero ECs.
3. **PR #838-era keys fail loud.** `provider = "host-signals"` (shipped by
   PR #838, deliberately not carried into this epic — providers spec §2)
   and `provider = "client-fixed"` are unknown keys and rejected like any
   other, so a config written against the PR #838 example cannot silently
   select a provider that no longer exists.
4. **Provider switches go through legacy readers.** Changing
   `[ec] provider` on a deployment with live identities requires listing
   the outgoing provider in `[ec] legacy_providers` (providers spec §6.1)
   so existing cookies keep resolving and stay withdrawable; the guide
   documents the switch sequence and the retirement/cleanup step that ends
   it.
5. **The example config ships the migrated happy path**, uncommented:
   `provider = "hmac"` with its block, `[geo] default_country`, and (for
   Fastly) the behavior-preserving `[device] provider = "fastly"` and
   `[geo] provider = "platform"` lines present with a comment stating what
   removing them changes. PR #838's example shipped the passphrase block
   uncommented with the selector commented out — steering operators directly
   into the silent-stateless state.
6. Every misconfiguration in the providers spec §6 table fails at
   **startup**. Request-time failure for a configuration error is a defect.
7. Config-store payload validation (`ts config push`) applies the same
   rules — including `[permissions]` policy validation (permission spec
   §3.3) — so a bad config is rejected at push time, before any instance
   restarts into it.

## 5. Behavior-preserving migration recipe (operator-facing)

The migration guide (a new `docs/guide/` page, linked from the release notes)
gives one copy-pasteable recipe per adapter for "keep exactly today's
behavior":

The recipe is the **complete recommended policy table from
`trusted-server.example.toml`** — the full GDPR/UK/US jurisdiction rules,
copied, not referenced by omission — plus this delta:

```toml
[ec]
provider = "hmac"
[ec.providers.hmac]
passphrase = "<existing passphrase>"

[device]
provider = "fastly"    # Fastly deployments: preserves the JA4 bot gate

[geo]
provider = "platform"  # preserves per-request jurisdiction detection
default_country = "FR" # used only when the host lookup fails (fail-closed
                       # because FR resolves to the gdpr-eu rule below)

# ... the full example policy table goes here: gdpr-eu / gdpr-uk /
# us-opt-out groups and their country rules, verbatim ...

# Delta vs. the protective example: preserve today's treatment of countries
# outside the regulation lists ("non-regulated" → identity allowed). Keep
# the example's `default = "gdpr-eu"` instead to require a signal worldwide
# (§2 row 7).
[permissions.groups.non-regulated]
regime = "none"
default = "granted"

[permissions.rules]
default = "non-regulated"
```

A partial policy is a trap the first draft of this spec fell into: a
`[permissions]` section containing **only** the permissive
default — with no GDPR/US rules — sends _every_ jurisdiction, France
included, to the permissive fallback, because `default_country` selects a
rule like any other country and finds none. The recipe therefore always
carries the full table, and CI pins it: **the exact documented recipe text
is a fixture**, loaded and run through the complete §4.1/§4.2 decision
matrix of the permission spec, asserting per-jurisdiction outcomes match
the pre-epic gate for every preservation row of §2.

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
   silent grant to everyone), not an error rate.
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
