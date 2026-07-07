# EdgeZero Migration Runbook

Operational reference for the Fastly Compute EdgeZero migration
(issue [#500](https://github.com/IABTechLab/trusted-server/issues/500),
epic [#480](https://github.com/IABTechLab/trusted-server/issues/480)).

---

## Current Status

The Fastly legacy cleanup has removed `legacy_main()` and the runtime canary
flag plumbing. The Fastly entry point now always dispatches through
`edgezero_main()`.

The historical canary keys are no longer read:

- `edgezero_enabled`
- `edgezero_rollout_pct`

Do not use those keys for rollout or rollback after the legacy cleanup.

---

## Runtime Config Store

Config store name: **`trusted_server_config`** (Fastly service config store)

The store must exist because the Fastly entry point opens it before EdgeZero
dispatch and passes the handle to EdgeZero-backed platform services. The store
may be empty unless another feature adds keys to it.

| Condition                         | Effective behaviour                          |
| --------------------------------- | -------------------------------------------- |
| `trusted_server_config` exists    | Request dispatches through `edgezero_main()` |
| `trusted_server_config` is absent | Request returns `500 Internal Server Error`  |

Local Viceroy templates must define an inline `trusted_server_config` store even
when no keys are present.

---

## Rollback Procedure

Rollback is no longer controlled by runtime config store keys. To roll back the
legacy cleanup, use the normal Fastly deployment rollback path:

1. Re-activate a previous service version that still contains the dual-path
   entry point, or deploy a revert of the legacy cleanup change.
2. Verify health, error rate, timeout rate, p95 latency, and auction win-rate
   against the same baseline window used for cutover.
3. If rolling back to a pre-cleanup build, use the historical PR19 canary
   runbook before changing `edgezero_enabled` or `edgezero_rollout_pct`.

---

## Monitoring

After cleanup there is only one Fastly request path, so monitoring no longer
needs an EdgeZero-vs-legacy branch split. Watch aggregate service metrics:

- **Error rate:** `5xx / total_requests` by edge PoP
- **Latency p95:** service-wide
- **Auction win-rate:** downstream SSP reporting, compare same-day prior week
- **Timeout rate:** `504 / total_requests`

For local validation under Viceroy, use the integration-test fixtures or
`fastly compute serve` with a `trusted_server_config` store present.

---

## Historical Rollout

The staged canary progression (`1% -> 10% -> 50% -> 100%`) and instant rollback
via `edgezero_rollout_pct = "0"` applied only before legacy cleanup. Historical
details are preserved in
`docs/superpowers/plans/2026-05-21-pr19-cutover-canary-rollout.md`.

---

## Reference

- Spec: `docs/superpowers/specs/2026-03-19-edgezero-migration-design.md`
- Cutover plan: `docs/superpowers/plans/2026-05-21-pr19-cutover-canary-rollout.md`
- Legacy cleanup plan: `docs/superpowers/plans/2026-05-27-pr20-legacy-cleanup.md`
- Legacy cleanup: issue [#495](https://github.com/IABTechLab/trusted-server/issues/495)
