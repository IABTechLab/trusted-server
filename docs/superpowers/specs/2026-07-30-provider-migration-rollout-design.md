# Design Spec: Provider and Permission Model — Migration and Rollout

**Status:** Draft
**Author:** Engineering
**Issue references:** #777–#781 (epic)
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-permission-model-design.md`
**Last updated:** 2026-07-30

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

| #   | Decision (today)                                                                                                                  | After epic                                                                                                                                                                                                                   | Status                                              |
| --- | --------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| 1   | EU/GDPR request, no TCF consent → no EC                                                                                           | Same (opt-in baseline)                                                                                                                                                                                                       | Preserved                                           |
| 2   | US-state request, GPC/GPP/USP opt-out → no EC, existing EC expired + tombstoned, **even when a consenting TCF string is present** | Same (precedence §4 of permission spec)                                                                                                                                                                                      | Preserved — **must not regress**                    |
| 3   | US-state request, no signals at all → no EC (fail-closed)                                                                         | Policy decision: the shipped `US` baseline decides. If the baseline grants `store-on-device` without a signal, that is a **declared change** requiring sign-off in the policy file review, with rationale in the file itself | Declared change (if made)                           |
| 4   | UK request, no TCF record → no EC                                                                                                 | Same, unless the policy file deliberately adopts a `granted` storage baseline for GB, with citation and sign-off                                                                                                             | Declared change (if made)                           |
| 5   | Unknown jurisdiction (geo unavailable) → no EC (fail-closed)                                                                      | `default_country` baseline, constrained by permission spec §5.3 so the fail-open combination cannot occur silently                                                                                                           | Declared change, guarded                            |
| 6   | Non-regulated country, TCF record refusing Purpose 1 → EC allowed, never tombstoned                                               | Same (withdrawal only where baseline is opt-in; permission spec §4.2)                                                                                                                                                        | Preserved                                           |
| 7   | EID transmission requires storage + personalization consent where regulated                                                       | Same via `store-on-device` ∧ `select-personalised-ads`                                                                                                                                                                       | Preserved                                           |
| 8   | Fastly bot gate requires JA4 + platform class before KV-backed EC writes                                                          | Only with `[device] provider = "fastly"`; the `builtin` default is UA-only                                                                                                                                                   | Declared change with a documented restore step (§5) |
| 9   | Fastly always resolves geo per request                                                                                            | Only with `[geo] provider = "platform"`                                                                                                                                                                                      | Declared change with a documented restore step (§5) |

Rows 3 and 4 are policy decisions, not code decisions: they belong in the
`permissions.yaml` review, made explicitly by maintainers — not implied by an
implementation.

## 3. Identity stability guarantee

For a deployment that selects `provider = "hmac"` and carries its passphrase
over verbatim:

- The minted identifier is **bit-identical** to today's:
  `HMAC-SHA256(passphrase, normalized_ip)` in the existing encoding.
- Cookie name, attributes, and max-age are unchanged; existing `ts-ec`
  cookies are recognized by the provider.
- KV identity-graph keys (`ec_hash`, normalization) are unchanged; no
  existing graph row is orphaned.

Enforced by **pinned known-answer tests**: fixed passphrase + IP → exact
expected identifier, cookie string, and KV key, committed as vectors so any
divergence fails CI rather than rotating a production identity base.

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
2. **Half-migrated fails loud.** A `[ec.providers.hmac]` block with no
   `provider = "hmac"` selector is a startup error (providers spec §6). In
   PR #838 this configuration — the exact state an operator following the
   docs reaches if they miss one line — validated green and silently minted
   zero ECs.
3. **The example config ships the migrated happy path**, uncommented:
   `provider = "hmac"` with its block, `[geo] default_country`, and (for
   Fastly) the behavior-preserving `[device] provider = "fastly"` and
   `[geo] provider = "platform"` lines present with a comment stating what
   removing them changes. PR #838's example shipped the passphrase block
   uncommented with the selector commented out — steering operators directly
   into the silent-stateless state.
4. Every misconfiguration in the providers spec §6 table fails at
   **startup**. Request-time failure for a configuration error is a defect.
5. Config-store payload validation (`ts config push`) applies the same
   rules, so a bad config is rejected at push time, before any instance
   restarts into it.

## 5. Behavior-preserving migration recipe (operator-facing)

The migration guide (a new `docs/guide/` page, linked from the release notes)
gives one copy-pasteable recipe per adapter for "keep exactly today's
behavior":

```toml
[ec]
provider = "hmac"
[ec.providers.hmac]
passphrase = "<existing passphrase>"

[device]
provider = "fastly"    # Fastly deployments: preserves the JA4 bot gate

[geo]
provider = "platform"  # preserves per-request jurisdiction detection
default_country = "FR" # used only when the host lookup fails
```

and separately documents the neutral configuration and what it does _not_ do.
The guide states explicitly that `default_country` alone does not replace geo
lookup, and why the permissive-default + no-geo combination requires the
explicit acknowledgment flag (permission spec §5.3).

## 6. Rollout sequence and observability

1. Implementation PRs land in the epic's order (providers first, permission
   model second); each is reviewable against §2 in isolation.
2. Before/after deploy, operators watch **EC issuance rate** and EID
   attachment rate; the migration guide names these as the canary metrics,
   because the failure mode of a bad migration is a silent drop to zero (or a
   silent grant to everyone), not an error rate.
3. Startup logs always print: selected provider per concern, whether geo is
   live, the effective default baseline, and the count of granted-without-
   signal permissions. One line, greppable, stable format.
4. Rollback is config-only where possible: reverting to the previous
   config version restores the previous behavior on the previous binary. The
   one irreversible artifact is withdrawal tombstones — which is why §2 row 6
   (no tombstones without affirmative withdrawal in an opt-in jurisdiction)
   is non-negotiable.

## 7. Documentation deliverables

- Migration guide page (§5), linked from `CHANGELOG.md` and the release
  notes.
- `configuration.md` documents **every** valid `provider` value for all
  three concerns, and documents environment-variable overrides only if they
  actually work in production builds (in PR #838 the documented
  `TRUSTED_SERVER__EC__PROVIDER` override existed only under `#[cfg(test)]`).
- The permission model page states the §4 precedence rules of the permission
  spec verbatim — operator docs and normative spec must not diverge on
  precedence, and prose like "signals are mapped as a grant or a revoke"
  without stating which wins is insufficient.
