# EdgeZero Migration Runbook

Operational reference for the Fastly Compute EdgeZero canary rollout
(issue [#500](https://github.com/IABTechLab/trusted-server/issues/500),
epic [#480](https://github.com/IABTechLab/trusted-server/issues/480)).

---

## Config store keys

Config store name: **`trusted_server_config`** (Fastly service config store)

| Key                    | Type                 | Effect                                                                                                                            |
| ---------------------- | -------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `edgezero_enabled`     | `"true"` / `"false"` | Master on/off switch. Set `"false"` to disable EdgeZero entirely, regardless of rollout_pct.                                      |
| `edgezero_rollout_pct` | `"0"` – `"100"`      | Percentage of traffic (by client IP bucket) routed to EdgeZero. Only read when `edgezero_enabled = "true"`. Key absent = `"100"`. |

**Routing logic:** `fnv1a_bucket(client_ip) < edgezero_rollout_pct` → EdgeZero, else legacy.
Same client IP always gets the same bucket — routing is sticky per user.

### Safe defaults / failure modes

| Condition                                           | Effective behaviour   |
| --------------------------------------------------- | --------------------- |
| Config store unreachable                            | All legacy            |
| `edgezero_enabled` unreadable                       | All legacy            |
| `edgezero_rollout_pct` absent (but enabled=true)    | All EdgeZero (100%)   |
| `edgezero_rollout_pct` invalid (non-integer, > 100) | All legacy            |
| `edgezero_rollout_pct = "0"`                        | All legacy (rollback) |

> ⚠️ **Do NOT delete `edgezero_rollout_pct` while `edgezero_enabled = "true"`.** An absent key
> is treated as 100 (full rollout) for backward compatibility. If you want to pause or roll back,
> **set the value to `"0"`** — do not delete it.

---

## Canary progression

> **Pre-condition:** All Phase 5 verification gates (PR18) passed.
>
> **Production key setup order (important):** Set `edgezero_rollout_pct = "0"` in the
> production config store **before** setting `edgezero_enabled = "true"`. If you set
> `edgezero_enabled` first and `edgezero_rollout_pct` is absent, the absent-key default
> (100) kicks in immediately, routing all traffic to EdgeZero without a staged canary.

### Pre-flight activation

Before advancing any stage, activate the canary switch:

1. Confirm `edgezero_rollout_pct = "0"` is already set in the production config store
   (set it now if not — the pre-condition above explains why this must come first).
2. Set `edgezero_enabled = "true"` in the production config store.
3. Confirm the flag is live and all traffic is still on the legacy path.
   `rollout_pct = "0"` deterministically short-circuits every request to the
   legacy path (`should_route_to_edgezero` in the Fastly entry point), so all-legacy
   is guaranteed by the config value rather than observed per request: confirm the
   config-store values are applied and that error rate, p95 latency, and timeout
   rate hold at baseline. There is no production per-branch route signal to tail
   (see [Monitoring](#monitoring)); the `routing request through legacy path
(rollout_pct=0)` line is emitted at `debug!` and is visible only in local
   Viceroy runs.

### Stage 1 — 1%

1. Set `edgezero_rollout_pct = "1"` in the production config store.
2. Hold **30 minutes**.
3. Check pass/fail thresholds (see below).
4. If all green → advance to Stage 2. If any threshold breached → rollback.

### Stage 2 — 10%

1. Set `edgezero_rollout_pct = "10"`.
2. Hold **2 hours** (same time-of-day window as the 7-day baseline).
3. Check pass/fail thresholds.
4. If all green → advance to Stage 3. If any threshold breached → rollback.

### Stage 3 — 50%

1. Set `edgezero_rollout_pct = "50"`.
2. Hold **24 hours**.
3. Check pass/fail thresholds. Pay particular attention to auction win-rate.
4. If all green → advance to Stage 4. If any threshold breached → rollback.

### Stage 4 — 100% (full cutover)

1. Set `edgezero_rollout_pct = "100"`.
2. Hold **48 hours** before decommissioning the legacy entry point.
3. Confirm zero regressions across all metrics.
4. Open legacy cleanup PR (removes `legacy_main()` and flag plumbing, see issue #495).

---

## Pass/fail thresholds

**Baseline definition:** 7-day rolling average from production Fastly service
metrics, sampled from the same time-of-day window as the canary observation
period (to account for diurnal traffic patterns).

| Metric           | Threshold                | Action if breached                     |
| ---------------- | ------------------------ | -------------------------------------- |
| Error rate (5xx) | > 0.1% above baseline    | **Immediate rollback**                 |
| p95 latency      | > 15% above baseline     | Hold; rollback if no fix within 1 hour |
| Auction win-rate | > 1% delta from baseline | Hold; investigate                      |
| Timeout rate     | > 2× baseline            | **Immediate rollback**                 |

> **Note on p95 threshold:** The spec §Cutover paragraph mentions ±10% as the Stage 2 hold-point
> criterion; the threshold table at §Pass/fail thresholds says 15%. These two values are
> inconsistent in the spec. This runbook adopts the threshold table (15%) as the governing
> number because it applies uniformly across all stages. If ops adopts a stricter 10% target
> at Stage 2, update this table accordingly.

---

## Rollback procedure

Rollback is **immediate, no deploy required**.

1. Set `edgezero_rollout_pct = "0"` in the production config store.
   Traffic shifts back to legacy within a few seconds as the config store propagates
   across edge PoPs; each Wasm instance picks up the change on its next request.
2. Optionally set `edgezero_enabled = "false"` as belt-and-suspenders.
3. Investigate root cause before re-advancing the canary.
4. Keep the legacy entry point (`legacy_main()`) available until at least one
   full release cycle after reaching 100% with zero regressions.

---

## Monitoring

> **There is no production signal that splits traffic by EdgeZero-vs-legacy
> branch yet.** The per-request route decision is emitted only at `log::debug!`
> (`should_route_to_edgezero` in the Fastly entry point), and the Fastly logger is
> pinned to `Info` (`logging::init_logger`, no runtime override), so these lines
> never reach the production log endpoint — they are observable only in local
> Viceroy runs. No `x-edgezero-path` response-path marker exists (deferred
> follow-up), and no Fastly real-time-stats traffic split is configured for this
> decision. Until a production-safe per-branch signal is added, canary
> verification relies on aggregate service metrics moving as expected when
> `rollout_pct` is stepped — not on per-request branch attribution.

Fastly real-time stats dashboard — aggregate service signals (not split by
branch). Watch each as `rollout_pct` is increased stage by stage; a regression
that appears and tracks the rollout steps implicates the EdgeZero branch:

- **Error rate:** `5xx / total_requests` by edge PoP
- **Latency p95:** service-wide
- **Auction win-rate:** downstream SSP reporting, compare same-day prior week
- **Timeout rate:** `504 / total_requests`

> For local pre-production validation under Viceroy (where `debug!` is visible),
> the route-decision log lines are:
>
> - `routing request through EdgeZero path (bucket=N, rollout_pct=M)` — partial-stage canary traffic.
> - `routing request through legacy path (bucket=N, rollout_pct=M)` — partial-stage legacy traffic.
> - At the degenerate values the bucket is not computed:
>   `routing request through legacy path (rollout_pct=0)` (full rollback) and
>   `routing request through EdgeZero path (rollout_pct=100)` (full cutover).

---

## Reference

- Spec: `docs/superpowers/specs/2026-03-19-edgezero-migration-design.md` §Cutover plan
- Plan: `docs/superpowers/plans/2026-05-21-pr19-cutover-canary-rollout.md`
- Legacy cleanup: issue [#495](https://github.com/IABTechLab/trusted-server/issues/495)
